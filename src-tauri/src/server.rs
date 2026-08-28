use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

use crate::hardware::{suggest_config_with_layers, SystemInfo};

/// File tools offered across llama.cpp builds. Some builds add extra tools
/// (apply_diff, get_datetime) which we don't offer — enabling an unknown tool
/// makes llama-server fail to start, so anything not listed here is dropped.
const KNOWN_TOOLS: [&str; 7] = [
    "read_file",
    "file_glob_search",
    "grep_search",
    "exec_shell_command",
    "write_file",
    "edit_file",
    "get_info",
];

/// Canonical `--tools` value for the given selection. Mirrors the frontend's
/// `sanitizeTools`: `"all"` expands to every known tool, the fully-selected set
/// collapses back to `"all"`, and unknown names are dropped. Returns `None`
/// when nothing valid remains (the flag should be omitted).
pub fn sanitize_tools(value: &str) -> Option<String> {
    if value.trim().is_empty() {
        return None;
    }
    if value.trim().eq_ignore_ascii_case("all") {
        return Some("all".to_string());
    }
    let mut sel: Vec<&str> = KNOWN_TOOLS
        .iter()
        .copied()
        .filter(|t| value.split(',').map(str::trim).any(|v| v == *t))
        .collect();
    if sel.is_empty() {
        return None;
    }
    sel.sort_unstable();
    if sel.len() == KNOWN_TOOLS.len() {
        return Some("all".to_string());
    }
    Some(sel.join(","))
}

/// Whether a tool name is supported by the known cross-build set.
pub fn is_known_tool(name: &str) -> bool {
    KNOWN_TOOLS.contains(&name)
}

/// Canonical `--tools` value for an explicit selection list (the app-wide
/// `server_tools` config). Returns `None` when nothing is selected (flag
/// omitted).
pub fn tool_arg_value(tools: &[String]) -> Option<String> {
    sanitize_tools(&tools.join(","))
}

/// Apply the app-wide file-tool selection to a server config. This is
/// authoritative: any `tools` value carried by a preset or the session config
/// is overridden so stale/unsupported names can't abort startup.
pub fn apply_global_tools(config: &mut ServerConfig, tools: &[String]) {
    match tool_arg_value(tools) {
        Some(value) => {
            config.extra_params.insert("tools".to_string(), value);
        }
        None => {
            config.extra_params.remove("tools");
        }
    }
}

/// `--mcp-servers-config` argument pair when MCP servers are configured.
pub fn mcp_args(mcp_config_path: Option<&std::path::Path>) -> Vec<String> {
    match mcp_config_path {
        Some(p) if !p.as_os_str().is_empty() => vec![
            "--mcp-servers-config".to_string(),
            p.to_string_lossy().to_string(),
        ],
        _ => Vec::new(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub model_path: String,
    #[serde(default)]
    pub mmproj_path: Option<String>,
    pub host: String,
    pub port: u16,
    // Context and memory
    pub n_ctx: u32,
    pub n_gpu_layers: i32,
    pub n_threads: Option<i32>,
    // Attention
    pub flash_attn: String,
    pub cache_type_k: String,
    pub cache_type_v: String,
    // Sampling
    pub temperature: f32,
    pub top_k: i32,
    pub min_p: f32,
    pub top_p: f32,
    pub n_predict: i32,
    // Batching
    pub n_batch: u32,
    pub n_ubatch: u32,
    pub cont_batching: bool,
    // Memory
    pub mlock: bool,
    pub no_mmap: bool,
    // Misc
    pub seed: Option<u64>,
    pub rope_freq_scale: Option<f32>,
    pub rope_freq_base: Option<f32>,
    pub grp_attn_n: Option<u32>,
    pub grp_attn_w: Option<u32>,
    // Slots
    pub parallel: u32,
    /// Working directory for the llama-server child process. Controls where the
    /// LLM's file tools (read_file/write_file/...) create and modify files.
    #[serde(default)]
    pub working_dir: Option<String>,
    // Additional CLI parameters: key = flag name (without --), value = argument (empty for boolean flags)
    #[serde(default)]
    pub extra_params: HashMap<String, String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            model_path: String::new(),
            mmproj_path: None,
            host: "127.0.0.1".to_string(),
            port: 8080,
            n_ctx: 0,
            n_gpu_layers: -1,
            n_threads: None,
            flash_attn: "auto".to_string(),
            cache_type_k: "f16".to_string(),
            cache_type_v: "f16".to_string(),
            temperature: 0.8,
            top_k: 40,
            min_p: 0.05,
            top_p: 0.95,
            n_predict: -1,
            n_batch: 512,
            n_ubatch: 512,
            cont_batching: true,
            mlock: false,
            no_mmap: false,
            seed: None,
            rope_freq_scale: None,
            rope_freq_base: None,
            grp_attn_n: None,
            grp_attn_w: None,
            parallel: 1,
            working_dir: None,
            extra_params: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ServerStatus {
    Stopped,
    Starting,
    Running { port: u16, pid: u32 },
    Error { message: String },
}

pub struct ServerState {
    pub process: Option<Child>,
    pub status: ServerStatus,
    pub log_lines: Vec<String>,
    /// Config of the currently running server (cleared when it stops).
    pub config: Option<ServerConfig>,
}

impl ServerState {
    pub fn new() -> Self {
        Self {
            process: None,
            status: ServerStatus::Stopped,
            log_lines: Vec::new(),
            config: None,
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(self.status, ServerStatus::Running { .. } | ServerStatus::Starting)
    }
}

pub type SharedServerState = Arc<Mutex<ServerState>>;

pub fn new_server_state() -> SharedServerState {
    Arc::new(Mutex::new(ServerState::new()))
}

/// Apply `HfPresetParams` fields to an existing `ServerConfig`.
/// Only overwrites fields that are present in the preset.
pub fn apply_hf_preset_params(params: &crate::huggingface::HfPresetParams, config: &mut ServerConfig) {
    if let Some(v) = params.temperature { config.temperature = v; }
    if let Some(v) = params.top_k { config.top_k = v; }
    if let Some(v) = params.top_p { config.top_p = v; }
    if let Some(v) = params.min_p { config.min_p = v; }
    if let Some(v) = params.n_predict { config.n_predict = v; }
    if let Some(v) = params.seed { config.seed = Some(v); }
    if let Some(v) = params.repeat_penalty {
        config.extra_params.insert("repeat-penalty".to_string(), format!("{:.4}", v));
    }
    if let Some(v) = params.repeat_last_n {
        config.extra_params.insert("repeat-last-n".to_string(), v.to_string());
    }
}

/// Derive a safe preset name from a HuggingFace repo_id (e.g. "unsloth/Foo" → "unsloth__Foo").
pub fn preset_name_from_repo(repo_id: &str) -> String {
    repo_id.replace('/', "__")
}

/// Rename/drop flag keys that were removed or renamed in newer llama.cpp builds.
/// Idempotent and safe to call on any `extra_params` map. Returns true if any
/// changes were made.
pub fn migrate_extra_params(extra: &mut HashMap<String, String>) -> bool {
    // Removed entirely (no automatic equivalent — meaning depended on --spec-type
    // which would now need user attention). Drop them so the server doesn't
    // refuse to start with an "argument has been removed" error.
    const REMOVED_DROP: &[&str] = &[
        "spec-ngram-size-n",
        "spec-ngram-size-m",
        "spec-ngram-min-hits",
    ];
    // Old → canonical rename. Some are still recognized by llama.cpp as aliases,
    // but we normalise to the canonical form so the UI and saved presets stay
    // consistent.
    const RENAMES: &[(&str, &str)] = &[
        // Removed entirely — must be migrated
        ("draft", "spec-draft-n-max"),
        ("draft-max", "spec-draft-n-max"),
        ("draft-n-max", "spec-draft-n-max"),
        ("draft-min", "spec-draft-n-min"),
        ("draft-n-min", "spec-draft-n-min"),
        // Still accepted as aliases — normalise to canonical
        ("model-draft", "spec-draft-model"),
        ("ctx-size-draft", "spec-draft-ctx-size"),
        ("n-gpu-layers-draft", "spec-draft-ngl"),
        ("gpu-layers-draft", "spec-draft-ngl"),
        ("device-draft", "spec-draft-device"),
        ("threads-draft", "spec-draft-threads"),
        ("threads-batch-draft", "spec-draft-threads-batch"),
        ("cpu-moe-draft", "spec-draft-cpu-moe"),
        ("draft-cpu-moe", "spec-draft-cpu-moe"),
        ("n-cpu-moe-draft", "spec-draft-n-cpu-moe"),
        ("override-tensor-draft", "spec-draft-override-tensor"),
        ("draft-p-min", "spec-draft-p-min"),
        ("draft-p-split", "spec-draft-p-split"),
        ("hf-repo-draft", "spec-draft-hf"),
        ("cache-type-k-draft", "spec-draft-type-k"),
        ("cache-type-v-draft", "spec-draft-type-v"),
    ];

    let mut changed = false;
    for k in REMOVED_DROP {
        if extra.remove(*k).is_some() {
            changed = true;
        }
    }
    for (old, new) in RENAMES {
        if let Some(v) = extra.remove(*old) {
            // Don't clobber an explicitly-set canonical value
            extra.entry((*new).to_string()).or_insert(v);
            changed = true;
        }
    }
    changed
}

pub async fn start_server(
    server_binary: &PathBuf,
    config: &ServerConfig,
    state: SharedServerState,
    mcp_config_path: Option<&PathBuf>,
    log_cb: impl Fn(String) + Send + Sync + 'static,
) -> Result<()> {
    // Check not already running
    {
        let s = state.lock().unwrap();
        if s.is_running() {
            anyhow::bail!("Server is already running");
        }
    }

    let mut args = build_args(config);
    args.extend(mcp_args(mcp_config_path.map(|p| p.as_path())));

    let cmdline = format!("{} {}", server_binary.display(), args.join(" "));
    log::info!("Starting llama-server: {}", cmdline);

    let mut cmd = Command::new(server_binary);
    cmd.args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    // Working directory for the server process — this is where the LLM's file
    // tools (read_file/write_file/edit_file/exec_shell_command) resolve relative
    // paths and create/modify files. Create it if it doesn't exist yet.
    if let Some(dir) = config.working_dir.as_deref().filter(|d| !d.is_empty()) {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("Failed to create working directory: {}", dir))?;
        cmd.current_dir(dir);
    }
    #[cfg(target_os = "windows")]
    {
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn()
        .context("Failed to spawn llama-server")?;

    let pid = child.id().unwrap_or(0);
    let port = config.port;

    // Take stdout/stderr before storing child in state
    let stdout = child.stdout.take().expect("stdout not piped");
    let stderr = child.stderr.take().expect("stderr not piped");

    {
        let mut s = state.lock().unwrap();
        s.process = Some(child);
        s.status = ServerStatus::Starting;
        s.log_lines.clear();
        s.config = Some(config.clone());
        // Add commandline as first log entry (after clear)
        s.log_lines.push(format!("$ {}", cmdline));
    }

    // Emit commandline as first log event
    log_cb(format!("$ {}", cmdline));

    // Read stdout/stderr in background tasks
    let state_clone = state.clone();
    let log_cb = Arc::new(log_cb);
    let log_cb_clone = log_cb.clone();
    let log_cb_exit = log_cb.clone();

    tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf).await {
                Ok(0) => break, // EOF
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf).trim_end().to_string();
                    let mut s = state_clone.lock().unwrap();
                    s.log_lines.push(line.clone());
                    if s.log_lines.len() > 500 {
                        s.log_lines.drain(0..100);
                    }
                    // Detect server ready
                    if matches!(s.status, ServerStatus::Starting)
                        && (line.contains("HTTP server listening")
                            || line.contains("server is listening")
                            || line.contains("listening on"))
                    {
                        s.status = ServerStatus::Running { port, pid };
                        log::info!("Server ready on port {}", port);
                    }
                    drop(s);
                    log_cb(line);
                }
                Err(e) => {
                    log::warn!("Error reading server stdout: {}", e);
                    break;
                }
            }
        }
    });

    let state_clone2 = state.clone();
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf).await {
                Ok(0) => break, // EOF
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf).trim_end().to_string();
                    let mut s = state_clone2.lock().unwrap();
                    s.log_lines.push(format!("[stderr] {}", line));
                    if s.log_lines.len() > 500 {
                        s.log_lines.drain(0..100);
                    }
                    if matches!(s.status, ServerStatus::Starting)
                        && (line.contains("HTTP server listening")
                            || line.contains("server is listening")
                            || line.contains("listening on"))
                    {
                        s.status = ServerStatus::Running { port, pid };
                    }
                    log_cb_clone(format!("[stderr] {}", line));
                }
                Err(e) => {
                    log::warn!("Error reading server stderr: {}", e);
                    break;
                }
            }
        }
    });

    // Monitor process exit in background via polling
    let state_clone3 = state.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;

            let exit_status = {
                let mut s = state_clone3.lock().unwrap();
                match s.process.as_mut() {
                    Some(child) => child.try_wait(),
                    None => break, // Child was taken by stop_server
                }
            };

            match exit_status {
                Ok(Some(status)) => {
                    let mut s = state_clone3.lock().unwrap();
                    s.process = None;
                    s.config = None;
                    if s.status == ServerStatus::Stopped {
                        // Already marked stopped by stop_server
                    } else if status.success() {
                        s.status = ServerStatus::Stopped;
                    } else {
                        let msg = format!("Server exited with code {}", status);
                        s.log_lines.push(format!("[error] {}", msg));
                        s.status = ServerStatus::Error { message: msg.clone() };
                        drop(s);
                        log_cb_exit(format!("[error] {}", msg));
                    }
                    break;
                }
                Ok(None) => continue, // Still running
                Err(e) => {
                    let mut s = state_clone3.lock().unwrap();
                    s.process = None;
                    s.config = None;
                    let msg = format!("Server process error: {}", e);
                    s.log_lines.push(format!("[error] {}", msg));
                    s.status = ServerStatus::Error { message: msg.clone() };
                    drop(s);
                    log_cb_exit(format!("[error] {}", msg));
                    break;
                }
            }
        }
    });

    Ok(())
}

pub async fn stop_server(state: &SharedServerState) -> Result<()> {
    let mut child = {
        let mut s = state.lock().unwrap();
        s.status = ServerStatus::Stopped;
        s.config = None;
        s.process.take()
    };

    if let Some(ref mut child) = child {
        // Send SIGTERM for graceful shutdown on Unix
        #[cfg(unix)]
        if let Some(pid) = child.id() {
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
            log::info!("Sent SIGTERM to server (pid {})", pid);
        }

        // On Windows, start_kill sends TerminateProcess immediately
        #[cfg(not(unix))]
        {
            let _ = child.start_kill();
        }

        // Wait up to 30 seconds for graceful shutdown
        match tokio::time::timeout(
            std::time::Duration::from_secs(30),
            child.wait(),
        )
        .await
        {
            Ok(Ok(status)) => {
                log::info!("Server exited: {}", status);
            }
            Ok(Err(e)) => {
                log::warn!("Error waiting for server exit: {}", e);
            }
            Err(_) => {
                // Timed out — force kill
                log::warn!("Server did not stop within 30 seconds, force killing");
                let _ = child.start_kill();
                let _ = child.wait().await;
            }
        }
    }

    Ok(())
}

/// Information about the currently running server, gathered from its HTTP
/// endpoints plus the config Catapult started it with.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    /// OpenAI-compatible base URL, e.g. http://127.0.0.1:8080/v1
    pub base_url: String,
    /// Model id as reported by /v1/models
    pub model_id: String,
    pub model_alias: String,
    pub model_path: String,
    pub n_ctx: u64,
    pub n_predict: i64,
    pub total_slots: u64,
    pub slots_idle: u64,
    pub api_key: Option<String>,
}

/// Fetch live info about the running server from its HTTP API.
pub async fn fetch_server_info(
    client: &reqwest::Client,
    port: u16,
    config: Option<&ServerConfig>,
) -> Result<ServerInfo> {
    let base = format!("http://127.0.0.1:{}", port);

    // /props — model path, alias, slot count
    let mut model_path = String::new();
    let mut model_alias = String::new();
    let mut total_slots: u64 = 1;
    if let Ok(resp) = client.get(format!("{}/props", base)).send().await {
        if let Ok(json) = resp.json::<serde_json::Value>().await {
            model_path = json.get("model_path").and_then(|v| v.as_str()).unwrap_or("").to_string();
            model_alias = json.get("model_alias").and_then(|v| v.as_str()).unwrap_or("").to_string();
            total_slots = json.get("total_slots").and_then(|v| v.as_u64()).unwrap_or(1);
        }
    }

    // /v1/models — model id for OpenAI clients
    let mut model_id = String::new();
    if let Ok(resp) = client.get(format!("{}/v1/models", base)).send().await {
        if let Ok(json) = resp.json::<serde_json::Value>().await {
            if let Some(data) = json.get("data").and_then(|v| v.as_array()) {
                if let Some(first) = data.first() {
                    model_id = first.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                }
            }
        }
    }
    if model_id.is_empty() {
        model_id = model_alias.clone();
    }

    // /slots — context size, n_predict, idle count
    let mut n_ctx: u64 = 0;
    let mut n_predict: i64 = -1;
    let mut slots_idle: u64 = 0;
    if let Ok(resp) = client.get(format!("{}/slots", base)).send().await {
        if let Ok(json) = resp.json::<serde_json::Value>().await {
            if let Some(slots) = json.as_array() {
                if let Some(first) = slots.first() {
                    n_ctx = first.get("n_ctx").and_then(|v| v.as_u64()).unwrap_or(0);
                    n_predict = first.get("n_predict").and_then(|v| v.as_i64()).unwrap_or(-1);
                }
                slots_idle = slots.iter().filter(|s| s.get("is_processing").and_then(|v| v.as_bool()).unwrap_or(true) == false).count() as u64;
            }
        }
    }

    let api_key = config
        .and_then(|c| c.extra_params.get("api-key").map(|k| k.clone()))
        .filter(|k| !k.is_empty());

    Ok(ServerInfo {
        base_url: format!("{}/v1", base),
        model_id,
        model_alias,
        model_path,
        n_ctx,
        n_predict,
        total_slots,
        slots_idle,
        api_key,
    })
}

/// Synchronous kill for use during app exit — sends SIGTERM/TerminateProcess
/// and waits briefly for the process to exit.
pub fn kill_server_sync(state: &SharedServerState) {
    let mut child = {
        let mut s = state.lock().unwrap();
        s.status = ServerStatus::Stopped;
        s.process.take()
    };

    if let Some(ref mut child) = child {
        #[cfg(unix)]
        if let Some(pid) = child.id() {
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
            log::info!("Sent SIGTERM to server on exit (pid {})", pid);
        }

        #[cfg(not(unix))]
        {
            let _ = child.start_kill();
        }

        // Block briefly to let the process clean up
        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_secs(5) {
            match child.try_wait() {
                Ok(Some(status)) => {
                    log::info!("Server exited on shutdown: {}", status);
                    return;
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(100)),
                Err(_) => break,
            }
        }

        // Force kill if still alive
        let _ = child.start_kill();
        let _ = child.try_wait();
        log::warn!("Force-killed server on shutdown");
    }
}

pub fn build_args(config: &ServerConfig) -> Vec<String> {
    let mut args = Vec::new();

    args.push("--model".to_string());
    args.push(config.model_path.clone());

    if let Some(ref mmproj) = config.mmproj_path {
        if !mmproj.is_empty() {
            args.push("--mmproj".to_string());
            args.push(mmproj.clone());
        }
    }

    args.push("--host".to_string());
    args.push(config.host.clone());

    args.push("--port".to_string());
    args.push(config.port.to_string());

    // --fit (default: on): llama-server auto-adjusts context size and GPU
    // layers to fit device memory. Explicit --ctx-size/--n-gpu-layers args
    // would block this, since fit only adjusts parameters not set by the user.
    let fit = config.extra_params.get("fit").map(|s| s.as_str()).unwrap_or("on");
    if fit == "off" {
        args.push("--ctx-size".to_string());
        args.push(config.n_ctx.to_string());
        args.push("--n-gpu-layers".to_string());
        args.push(config.n_gpu_layers.to_string());
        args.push("--fit".to_string());
        args.push("off".to_string());
    } else {
        args.push("--fit".to_string());
        args.push("on".to_string());
    }

    if let Some(threads) = config.n_threads {
        args.push("--threads".to_string());
        args.push(threads.to_string());
    }

    args.push("--flash-attn".to_string());
    args.push(config.flash_attn.clone());

    args.push("--cache-type-k".to_string());
    args.push(config.cache_type_k.clone());

    args.push("--cache-type-v".to_string());
    args.push(config.cache_type_v.clone());

    args.push("--temp".to_string());
    args.push(format!("{:.2}", config.temperature));

    args.push("--top-k".to_string());
    args.push(config.top_k.to_string());

    args.push("--min-p".to_string());
    args.push(format!("{:.4}", config.min_p));

    args.push("--top-p".to_string());
    args.push(format!("{:.4}", config.top_p));

    if config.n_predict != -1 {
        args.push("--n-predict".to_string());
        args.push(config.n_predict.to_string());
    }

    args.push("--batch-size".to_string());
    args.push(config.n_batch.to_string());

    args.push("--ubatch-size".to_string());
    args.push(config.n_ubatch.to_string());

    if config.cont_batching {
        args.push("--cont-batching".to_string());
    } else {
        args.push("--no-cont-batching".to_string());
    }

    if config.mlock {
        args.push("--mlock".to_string());
    }

    if config.no_mmap {
        args.push("--no-mmap".to_string());
    }

    if let Some(seed) = config.seed {
        args.push("--seed".to_string());
        args.push(seed.to_string());
    }

    if let Some(scale) = config.rope_freq_scale {
        args.push("--rope-freq-scale".to_string());
        args.push(format!("{:.6}", scale));
    }

    if let Some(base) = config.rope_freq_base {
        args.push("--rope-freq-base".to_string());
        args.push(format!("{:.1}", base));
    }

    if let Some(n) = config.grp_attn_n {
        args.push("--grp-attn-n".to_string());
        args.push(n.to_string());
    }

    if let Some(w) = config.grp_attn_w {
        args.push("--grp-attn-w".to_string());
        args.push(w.to_string());
    }

    args.push("--parallel".to_string());
    args.push(config.parallel.to_string());

    // Extra parameters from the UI
    let mut sorted_params: Vec<_> = config.extra_params.iter()
        .filter(|(k, _)| k.as_str() != "__raw__" && k.as_str() != "mmproj" && k.as_str() != "fit")
        .collect();
    sorted_params.sort_by_key(|(k, _)| (*k).clone());
    for (key, value) in sorted_params {
        // `tools` is validated against the known set — stale/unsupported names
        // in saved configs or presets must not fail server startup.
        if key == "tools" {
            if let Some(clean) = sanitize_tools(value) {
                args.push("--tools".to_string());
                if !clean.is_empty() {
                    args.push(clean);
                }
            }
            continue;
        }
        args.push(format!("--{}", key));
        if !value.is_empty() {
            args.push(value.clone());
        }
    }

    // Raw extra arguments (free-form text from the UI)
    if let Some(raw) = config.extra_params.get("__raw__") {
        for part in raw.split_whitespace() {
            args.push(part.to_string());
        }
    }

    args
}

/// Build a suggested config based on system info and model size
pub fn suggest_server_config(
    model_path: &str,
    model_size_mb: u64,
    system: &SystemInfo,
) -> ServerConfig {
    // Read model architecture from the GGUF header when available
    let meta = crate::models::read_model_metadata(std::path::Path::new(model_path));
    let layers = meta.as_ref().and_then(|m| m.block_count).map(|l| l as u32);
    let suggestion = suggest_config_with_layers(model_size_mb, layers, system);

    let cache_type_k = if suggestion.can_fit_fully_in_vram || suggestion.total_usable_mb > 8192 {
        "f16".to_string()
    } else {
        "q8_0".to_string() // Save memory
    };

    // When the model doesn't fully fit in VRAM, pick the largest context that
    // fits in the VRAM left over after the offloaded weights (KV cache lives
    // on the GPU when offloading). Clamped to [4096, model context].
    let n_ctx = if suggestion.can_fit_fully_in_vram || suggestion.n_gpu_layers <= 0 {
        suggestion.n_ctx
    } else {
        let embd = meta.as_ref().and_then(|m| m.embedding_length).unwrap_or(4096);
        let model_ctx = meta.as_ref().and_then(|m| m.context_length).unwrap_or(131072);
        let gqa_factor = match (meta.as_ref().and_then(|m| m.attention_head_count),
                                meta.as_ref().and_then(|m| m.attention_head_count_kv)) {
            (Some(heads), Some(kv_heads)) if heads > 0 => kv_heads as f64 / heads as f64,
            _ => 1.0,
        };
        let kv_embd = (embd as f64 * gqa_factor).max(1.0) as u64;
        let layers_u64 = layers.map(|l| l as u64).unwrap_or(32);
        let kv_bytes_per_tok = crate::hardware::kv_bytes_per_token(layers_u64, kv_embd, &cache_type_k, "f16");

        let vram_total_mb: u64 = system.gpus.iter().map(|g| g.vram_mb).sum();
        // VRAM taken by offloaded weights (proportional to offload layers) + 512 MiB overhead + 1024 MiB margin
        let offload_ratio = suggestion.n_gpu_layers as f64 / layers_u64.max(1) as f64;
        let model_in_vram_mb = (model_size_mb as f64 * offload_ratio) as u64;
        let free_vram_mb = vram_total_mb.saturating_sub(model_in_vram_mb + 512 + 1024);

        let fitted = if kv_bytes_per_tok > 0 {
            let max_ctx = free_vram_mb * 1024 * 1024 / kv_bytes_per_tok;
            (max_ctx.clamp(4096, model_ctx) & !511) as u32
        } else {
            0
        };
        fitted
    };

    ServerConfig {
        model_path: model_path.to_string(),
        n_ctx,
        n_gpu_layers: suggestion.n_gpu_layers,
        flash_attn: "auto".to_string(),
        cache_type_k,
        cache_type_v: "f16".to_string(),
        ..ServerConfig::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_args_default_config() {
        let config = ServerConfig {
            model_path: "/path/to/model.gguf".to_string(),
            ..Default::default()
        };
        let args = build_args(&config);

        assert!(args.contains(&"--model".to_string()));
        assert!(args.contains(&"/path/to/model.gguf".to_string()));
        assert!(args.contains(&"--host".to_string()));
        assert!(args.contains(&"127.0.0.1".to_string()));
        assert!(args.contains(&"--port".to_string()));
        assert!(args.contains(&"8080".to_string()));
        // Default fit is ON: ctx-size/ngl are left to llama-server
        assert!(args.contains(&"--fit".to_string()));
        assert!(args.contains(&"on".to_string()));
        assert!(!args.contains(&"--ctx-size".to_string()));
        assert!(!args.contains(&"--n-gpu-layers".to_string()));
        assert!(args.contains(&"--flash-attn".to_string()));
        assert!(args.contains(&"auto".to_string()));
    }

    #[test]
    fn build_args_fit_off_passes_explicit_ctx_and_ngl() {
        let mut extra = HashMap::new();
        extra.insert("fit".to_string(), "off".to_string());
        let config = ServerConfig {
            model_path: "/m.gguf".to_string(),
            n_ctx: 8192,
            n_gpu_layers: 20,
            extra_params: extra,
            ..Default::default()
        };
        let args = build_args(&config);
        assert!(args.contains(&"--fit".to_string()));
        assert!(args.contains(&"off".to_string()));
        let ctx_idx = args.iter().position(|a| a == "--ctx-size").unwrap();
        assert_eq!(args[ctx_idx + 1], "8192");
        let ngl_idx = args.iter().position(|a| a == "--n-gpu-layers").unwrap();
        assert_eq!(args[ngl_idx + 1], "20");
    }

    #[test]
    fn build_args_optional_fields() {
        let config = ServerConfig {
            model_path: "/m.gguf".to_string(),
            n_threads: Some(8),
            seed: Some(42),
            parallel: 4,
            ..Default::default()
        };
        let args = build_args(&config);

        assert!(args.contains(&"--threads".to_string()));
        assert!(args.contains(&"8".to_string()));
        assert!(args.contains(&"--seed".to_string()));
        assert!(args.contains(&"42".to_string()));
        assert!(args.contains(&"--parallel".to_string()));
        assert!(args.contains(&"4".to_string()));
    }

    #[test]
    fn build_args_extra_params() {
        let mut extra = HashMap::new();
        extra.insert("api-key".to_string(), "secret".to_string());
        extra.insert("metrics".to_string(), String::new()); // boolean flag
        extra.insert("__raw__".to_string(), "--verbose --log-timestamps".to_string());

        let config = ServerConfig {
            model_path: "/m.gguf".to_string(),
            extra_params: extra,
            ..Default::default()
        };
        let args = build_args(&config);

        // Named extra params (sorted alphabetically)
        let api_key_idx = args.iter().position(|a| a == "--api-key").unwrap();
        assert_eq!(args[api_key_idx + 1], "secret");

        assert!(args.contains(&"--metrics".to_string()));

        // Raw args split and appended at the end
        let verbose_idx = args.iter().position(|a| a == "--verbose").unwrap();
        let timestamps_idx = args.iter().position(|a| a == "--log-timestamps").unwrap();
        assert!(verbose_idx > api_key_idx);
        assert!(timestamps_idx > verbose_idx);
    }

    #[test]
    fn build_args_parallel_always_emitted() {
        let config = ServerConfig {
            model_path: "/m.gguf".to_string(),
            parallel: 1,
            ..Default::default()
        };
        let args = build_args(&config);
        let idx = args.iter().position(|a| a == "--parallel").unwrap();
        assert_eq!(args[idx + 1], "1");
    }

    #[test]
    fn sanitize_tools_drops_unknown_names() {
        assert_eq!(
            sanitize_tools("file_glob_search,get_datetime,get_info"),
            Some("file_glob_search,get_info".to_string())
        );
        assert_eq!(sanitize_tools("get_datetime,zz_new"), None);
        assert_eq!(sanitize_tools(""), None);
        assert_eq!(sanitize_tools("  "), None);
    }

    #[test]
    fn sanitize_tools_handles_all_and_full_set() {
        assert_eq!(sanitize_tools("all"), Some("all".to_string()));
        assert_eq!(
            sanitize_tools(&KNOWN_TOOLS.join(",")),
            Some("all".to_string())
        );
        assert_eq!(
            sanitize_tools("read_file,write_file"),
            Some("read_file,write_file".to_string())
        );
    }

    #[test]
    fn build_args_drops_unknown_tools() {
        let mut extra = HashMap::new();
        extra.insert(
            "tools".to_string(),
            "file_glob_search,get_datetime,get_info".to_string(),
        );
        let config = ServerConfig {
            model_path: "/m.gguf".to_string(),
            extra_params: extra,
            ..Default::default()
        };
        let args = build_args(&config);
        let tools_idx = args.iter().position(|a| a == "--tools").unwrap();
        assert_eq!(args[tools_idx + 1], "file_glob_search,get_info");
        assert!(!args.contains(&"get_datetime".to_string()));
    }

    #[test]
    fn build_args_omits_tools_when_nothing_valid() {
        let mut extra = HashMap::new();
        extra.insert("tools".to_string(), "get_datetime,zz_new".to_string());
        let config = ServerConfig {
            model_path: "/m.gguf".to_string(),
            extra_params: extra,
            ..Default::default()
        };
        let args = build_args(&config);
        assert!(!args.contains(&"--tools".to_string()));
    }

    #[test]
    fn is_known_tool_accepts_only_cross_build_tools() {
        assert!(is_known_tool("read_file"));
        assert!(is_known_tool("exec_shell_command"));
        assert!(!is_known_tool("get_datetime"));
        assert!(!is_known_tool("zz_new"));
        assert!(!is_known_tool(""));
    }

    #[test]
    fn tool_arg_value_collapses_selection() {
        assert_eq!(tool_arg_value(&[]), None);
        assert_eq!(tool_arg_value(&["zz_new".to_string()]), None);
        assert_eq!(
            tool_arg_value(&["read_file".to_string(), "write_file".to_string()]),
            Some("read_file,write_file".to_string())
        );
        // All known tools collapse to "all"
        let all: Vec<String> = KNOWN_TOOLS.iter().map(|s| s.to_string()).collect();
        assert_eq!(tool_arg_value(&all), Some("all".to_string()));
    }

    #[test]
    fn apply_global_tools_sets_and_removes_tools_flag() {
        let mut config = ServerConfig {
            model_path: "/m.gguf".to_string(),
            ..Default::default()
        };
        // Stale preset value is replaced by the global selection
        config.extra_params.insert("tools".to_string(), "get_datetime".to_string());
        apply_global_tools(&mut config, &["read_file".to_string(), "write_file".to_string()]);
        assert_eq!(config.extra_params.get("tools").map(|s| s.as_str()), Some("read_file,write_file"));

        // Empty global selection removes the flag entirely
        apply_global_tools(&mut config, &[]);
        assert!(!config.extra_params.contains_key("tools"));
    }

    #[test]
    fn apply_global_tools_normalizes_full_selection_to_all() {
        let mut config = ServerConfig {
            model_path: "/m.gguf".to_string(),
            ..Default::default()
        };
        let all: Vec<String> = KNOWN_TOOLS.iter().map(|s| s.to_string()).collect();
        apply_global_tools(&mut config, &all);
        assert_eq!(config.extra_params.get("tools").map(|s| s.as_str()), Some("all"));
    }

    #[test]
    fn mcp_args_emits_flag_only_with_path() {
        assert_eq!(mcp_args(None), Vec::<String>::new());
        let path = PathBuf::from("C:/data/catapult/mcp.json");
        assert_eq!(
            mcp_args(Some(&path)),
            vec!["--mcp-servers-config".to_string(), "C:/data/catapult/mcp.json".to_string()]
        );
    }

    #[test]
    fn build_args_no_cont_batching() {
        let config = ServerConfig {
            model_path: "/m.gguf".to_string(),
            cont_batching: false,
            ..Default::default()
        };
        let args = build_args(&config);
        assert!(args.contains(&"--no-cont-batching".to_string()));
        assert!(!args.contains(&"--cont-batching".to_string()));
    }

    #[test]
    fn build_args_parallel_emitted_for_higher_values() {
        let config = ServerConfig {
            model_path: "/m.gguf".to_string(),
            parallel: 8,
            ..Default::default()
        };
        let args = build_args(&config);
        let idx = args.iter().position(|a| a == "--parallel").unwrap();
        assert_eq!(args[idx + 1], "8");
    }

    #[test]
    fn build_args_cont_batching_when_enabled() {
        let config = ServerConfig {
            model_path: "/m.gguf".to_string(),
            cont_batching: true,
            ..Default::default()
        };
        let args = build_args(&config);
        assert!(args.contains(&"--cont-batching".to_string()));
        assert!(!args.contains(&"--no-cont-batching".to_string()));
    }

    #[test]
    fn build_args_default_has_parallel_and_cont_batching() {
        // Default config: parallel=1, cont_batching=true
        let config = ServerConfig {
            model_path: "/m.gguf".to_string(),
            ..Default::default()
        };
        let args = build_args(&config);
        // parallel=1 must be emitted (not omitted)
        let idx = args.iter().position(|a| a == "--parallel").unwrap();
        assert_eq!(args[idx + 1], "1");
        // cont_batching=true emits --cont-batching
        assert!(args.contains(&"--cont-batching".to_string()));
    }

    #[test]
    fn build_args_omits_none_threads() {
        let config = ServerConfig {
            model_path: "/m.gguf".to_string(),
            n_threads: None,
            ..Default::default()
        };
        let args = build_args(&config);
        assert!(!args.contains(&"--threads".to_string()));
    }

    // ── kill_server_sync ─────────────────────────────────────────────────────

    #[test]
    fn kill_server_sync_no_process_is_noop() {
        let state = new_server_state();
        // Must not panic when no server is running
        kill_server_sync(&state);
        let s = state.lock().unwrap();
        assert!(matches!(s.status, ServerStatus::Stopped));
        assert!(s.process.is_none());
    }

    // ── apply_hf_preset_params ───────────────────────────────────────────────

    fn make_hf_params() -> crate::huggingface::HfPresetParams {
        crate::huggingface::HfPresetParams {
            temperature: Some(0.6),
            top_k: Some(30),
            top_p: Some(0.85),
            min_p: Some(0.02),
            n_predict: Some(1024),
            seed: Some(123),
            repeat_penalty: Some(1.15),
            repeat_last_n: Some(64),
        }
    }

    #[test]
    fn apply_hf_preset_updates_sampling_fields() {
        let mut cfg = ServerConfig::default();
        apply_hf_preset_params(&make_hf_params(), &mut cfg);

        assert!((cfg.temperature - 0.6).abs() < 1e-5);
        assert_eq!(cfg.top_k, 30);
        assert!((cfg.top_p - 0.85).abs() < 1e-5);
        assert!((cfg.min_p - 0.02).abs() < 1e-5);
        assert_eq!(cfg.n_predict, 1024);
        assert_eq!(cfg.seed, Some(123));
    }

    #[test]
    fn apply_hf_preset_puts_repeat_in_extra_params() {
        let mut cfg = ServerConfig::default();
        apply_hf_preset_params(&make_hf_params(), &mut cfg);

        assert!(cfg.extra_params.contains_key("repeat-penalty"),
            "repeat_penalty should be stored in extra_params");
        assert!(cfg.extra_params.contains_key("repeat-last-n"),
            "repeat_last_n should be stored in extra_params");
        let rp: f32 = cfg.extra_params["repeat-penalty"].parse().unwrap();
        assert!((rp - 1.15).abs() < 1e-3);
        assert_eq!(cfg.extra_params["repeat-last-n"], "64");
    }

    #[test]
    fn apply_hf_preset_none_fields_preserve_defaults() {
        let mut cfg = ServerConfig::default();
        let default_temp = cfg.temperature;
        let params = crate::huggingface::HfPresetParams::default(); // all None
        apply_hf_preset_params(&params, &mut cfg);

        // Nothing should have changed
        assert!((cfg.temperature - default_temp).abs() < 1e-5);
        assert!(cfg.extra_params.is_empty());
        assert_eq!(cfg.seed, None);
    }

    #[test]
    fn apply_hf_preset_does_not_touch_hardware_fields() {
        let mut cfg = ServerConfig {
            n_gpu_layers: 99,
            n_ctx: 4096,
            n_threads: Some(8),
            ..Default::default()
        };
        apply_hf_preset_params(&make_hf_params(), &mut cfg);
        // Hardware fields must be untouched
        assert_eq!(cfg.n_gpu_layers, 99);
        assert_eq!(cfg.n_ctx, 4096);
        assert_eq!(cfg.n_threads, Some(8));
    }

    // ── preset_name_from_repo ────────────────────────────────────────────────

    #[test]
    fn preset_name_from_repo_replaces_slash() {
        assert_eq!(preset_name_from_repo("unsloth/Qwen3.5-4B-GGUF"), "unsloth__Qwen3.5-4B-GGUF");
    }

    #[test]
    fn preset_name_from_repo_no_slash() {
        assert_eq!(preset_name_from_repo("plain-name"), "plain-name");
    }

    // ── migrate_extra_params ─────────────────────────────────────────────────

    #[test]
    fn migrate_drops_removed_ngram_size_flags() {
        let mut ep = HashMap::new();
        ep.insert("spec-ngram-size-n".to_string(), "3".to_string());
        ep.insert("spec-ngram-size-m".to_string(), "5".to_string());
        ep.insert("spec-ngram-min-hits".to_string(), "1".to_string());
        ep.insert("kept".to_string(), "1".to_string());

        assert!(migrate_extra_params(&mut ep));
        assert!(!ep.contains_key("spec-ngram-size-n"));
        assert!(!ep.contains_key("spec-ngram-size-m"));
        assert!(!ep.contains_key("spec-ngram-min-hits"));
        assert_eq!(ep.get("kept"), Some(&"1".to_string()));
    }

    #[test]
    fn migrate_renames_removed_draft_flags_to_spec_draft() {
        let mut ep = HashMap::new();
        ep.insert("draft".to_string(), "16".to_string());
        ep.insert("draft-min".to_string(), "0".to_string());

        assert!(migrate_extra_params(&mut ep));
        assert_eq!(ep.get("spec-draft-n-max"), Some(&"16".to_string()));
        assert_eq!(ep.get("spec-draft-n-min"), Some(&"0".to_string()));
        assert!(!ep.contains_key("draft"));
        assert!(!ep.contains_key("draft-min"));
    }

    #[test]
    fn migrate_canonicalises_draft_aliases() {
        let mut ep = HashMap::new();
        ep.insert("model-draft".to_string(), "/p/d.gguf".to_string());
        ep.insert("ctx-size-draft".to_string(), "4096".to_string());
        ep.insert("n-gpu-layers-draft".to_string(), "99".to_string());
        ep.insert("threads-draft".to_string(), "4".to_string());
        ep.insert("device-draft".to_string(), "cuda0".to_string());
        ep.insert("cpu-moe-draft".to_string(), String::new());

        assert!(migrate_extra_params(&mut ep));
        assert_eq!(ep.get("spec-draft-model"), Some(&"/p/d.gguf".to_string()));
        assert_eq!(ep.get("spec-draft-ctx-size"), Some(&"4096".to_string()));
        assert_eq!(ep.get("spec-draft-ngl"), Some(&"99".to_string()));
        assert_eq!(ep.get("spec-draft-threads"), Some(&"4".to_string()));
        assert_eq!(ep.get("spec-draft-device"), Some(&"cuda0".to_string()));
        assert_eq!(ep.get("spec-draft-cpu-moe"), Some(&String::new()));
        assert!(!ep.contains_key("model-draft"));
    }

    #[test]
    fn migrate_does_not_clobber_explicit_canonical_value() {
        let mut ep = HashMap::new();
        ep.insert("spec-draft-n-max".to_string(), "32".to_string());
        ep.insert("draft".to_string(), "16".to_string());

        migrate_extra_params(&mut ep);
        // Canonical wins; the legacy key is removed.
        assert_eq!(ep.get("spec-draft-n-max"), Some(&"32".to_string()));
        assert!(!ep.contains_key("draft"));
    }

    #[test]
    fn migrate_idempotent_on_clean_map() {
        let mut ep = HashMap::new();
        ep.insert("spec-default".to_string(), String::new());
        ep.insert("temp".to_string(), "0.7".to_string());
        let before = ep.clone();
        assert!(!migrate_extra_params(&mut ep));
        assert_eq!(ep, before);
    }
}
