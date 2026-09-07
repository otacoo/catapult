// ── Harness runtime + commands ──────────────────────────────────────────────
//
// Phase 1: the agent loop (orchestrator) with sandboxed tools, running against
// the managed llama-server. State lives in `HarnessRuntime` (shared via
// AppState): message history for the current session, the permission engine,
// an in-flight marker, and the pending approval channel the loop parks on.
//
// Phase 3: model roles — in router mode, the orchestrator and worker models
// are resolved against the router's registry, the models-preset is regenerated
// when needed, and role models are loaded on demand before the loop starts.
//
// Events to the UI flow through a typed `Channel` (stream + tool events) and
// a global `harness_approval` event for approval prompts (they can arrive
// while the invoke promise is still pending).

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, ipc::Channel, State};

use crate::AppState;
use harness::agent::{AgentEvent, AgentRun, ApprovalGate, ApprovalRequest, Approved};
use harness::client::{ChatMessage, LlmClient, StreamEvent};
use harness::permissions::PermissionEngine;
use harness::sandbox::PathJail;
use harness::tools::ToolRegistry;

/// Byte-stable system prompt (KV-cache friendly). The project line is appended
/// once and stays stable per project.
const SYSTEM_PROMPT: &str = "You are Catapult's agent, working inside a sandboxed project directory. \
File tools are rooted at that directory; relative paths resolve there. \
Read-only operations run automatically; writes and shell commands may require user approval — \
if denied, adapt instead of retrying the same call. \
Work step by step: read before editing, make small exact edits, verify results, \
and give a concise summary when done.";

pub struct HarnessRuntime {
    /// Current agent conversation (system prompt included once, byte-stable).
    pub history: Mutex<Vec<ChatMessage>>,
    pub engine: Arc<PermissionEngine>,
    /// Set while the agent loop is in flight; blocks concurrent sends.
    pub running: std::sync::atomic::AtomicBool,
    /// The parked approval the loop is waiting on (oneshot per request).
    pub pending: Mutex<Option<tokio::sync::oneshot::Sender<Approved>>>,
    /// Whether the persisted session was loaded this app run (loaded lazily
    /// on the first send so a fresh start resumes, but finished sessions
    /// don't resurrect mid-run).
    pub session_loaded: std::sync::atomic::AtomicBool,
    /// One-shot notices (e.g. VRAM feasibility) are shown once per app run.
    pub notice_shown: std::sync::atomic::AtomicBool,
    /// MCP server sessions + their tool listings (built once per app run;
    /// invalidated when the Tools page saves mcp.json).
    pub mcp: Mutex<Option<Arc<Vec<McpConnection>>>>,
    /// Id of the session file currently open (None = new one on next send).
    pub session_id: Mutex<Option<String>>,
    /// Prompt tokens of the last completed run — the freshest measure of
    /// context fill until the next run finishes.
    pub last_prompt_tokens: Mutex<Option<u64>>,
}

/// Shape of a persisted session file.
#[derive(Debug, Serialize, Deserialize)]
pub struct PersistedSession {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    pub created: i64,
    pub updated: i64,
    pub messages: Vec<ChatMessage>,
}

/// Listing entry for the Chat sidebar.
#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    pub id: String,
    pub title: String,
    pub updated: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

pub fn session_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("catapult").join("sessions"))
}

fn session_file(id: &str) -> Option<PathBuf> {
    session_dir().map(|d| d.join(format!("{}.json", id)))
}

/// Stable-ish id from a timestamp: `s<millis>`.
fn new_session_id() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("s{}", now)
}

fn session_title(history: &[ChatMessage]) -> String {
    let first_user = history
        .iter()
        .find(|m| m.role == "user")
        .and_then(|m| m.content.as_ref())
        .and_then(|c| c.as_str())
        .unwrap_or_default();
    let mut t: String = first_user.lines().next().unwrap_or("New chat").to_string();
    if t.chars().count() > 60 {
        t = t.chars().take(60).collect();
        t.push('…');
    }
    t
}

/// Read a persisted session; None when missing/corrupt.
fn read_session_file(path: &std::path::Path) -> Option<PersistedSession> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Resume the most recently updated session (app start).
fn load_session(state: &AppState) {
    let rt = &state.harness;
    if rt.session_loaded.swap(true, Ordering::SeqCst) {
        return;
    }
    let Some(dir) = session_dir() else { return };
    let mut best: Option<PersistedSession> = None;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if let Some(s) = read_session_file(&path) {
                let replace = match &best {
                    Some(b) => s.updated > b.updated,
                    None => true,
                };
                if replace {
                    best = Some(s);
                }
            }
        }
    }
    if let Some(s) = best {
        *rt.history.lock().unwrap() = s.messages;
        *rt.session_id.lock().unwrap() = Some(s.id);
    }
}

/// Save the current transcript under its id (creating the id if needed).
fn save_session(state: &AppState) {
    let Some(dir) = session_dir() else { return };
    let _ = std::fs::create_dir_all(&dir);
    let history = state.harness.history.lock().unwrap().clone();
    if history.iter().all(|m| m.role != "user") {
        return; // nothing worth persisting
    }
    let id = state
        .harness
        .session_id
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_else(new_session_id);
    *state.harness.session_id.lock().unwrap() = Some(id.clone());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let created = session_file(&id)
        .and_then(|p| read_session_file(&p))
        .map(|s| s.created)
        .unwrap_or(now);
    let project = {
        let c = state.config.lock().unwrap();
        c.harness_active_project
            .as_ref()
            .and_then(|id| c.harness_projects.iter().find(|p| &p.id == id))
            .map(|p| p.path.clone())
    };
    let session = PersistedSession {
        id: id.clone(),
        title: session_title(&history),
        project,
        created,
        updated: now,
        messages: history,
    };
    if let Some(file) = session_file(&id) {
        if let Ok(json) = serde_json::to_string(&session) {
            let _ = std::fs::write(file, json);
        }
    }
}

impl HarnessRuntime {
    pub fn new() -> Self {
        Self {
            history: Mutex::new(Vec::new()),
            engine: Arc::new(PermissionEngine::new()),
            running: std::sync::atomic::AtomicBool::new(false),
            pending: Mutex::new(None),
            session_loaded: std::sync::atomic::AtomicBool::new(false),
            notice_shown: std::sync::atomic::AtomicBool::new(false),
            mcp: Mutex::new(None),
            session_id: Mutex::new(None),
            last_prompt_tokens: Mutex::new(None),
        }
    }
}

/// Live context stats for the ring: fill from the last completed run, ceiling
/// from the live slot size (`GET /slots`, which reflects the server's actual
/// per-slot context), falling back to the GGUF metadata length.
#[derive(Debug, Serialize)]
pub struct ContextStats {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
}

#[tauri::command]
pub async fn harness_context_stats(state: State<'_, AppState>) -> Result<ContextStats, String> {
    let used = *state.harness.last_prompt_tokens.lock().unwrap();
    let port = port_or_err(&state)?;
    let client = LlmClient::new(format!("http://127.0.0.1:{port}"));
    // Live ceiling first; the GGUF length is only a fallback (it can exceed
    // what --fit actually chose, and says nothing in router mode).
    let mut total = client.slot_context().await.unwrap_or(None);
    if total.is_none() {
        if let Some(path) = active_model_path(&state) {
            total = crate::models::read_model_metadata(std::path::Path::new(&path))
                .and_then(|m| m.context_length);
        }
    }
    Ok(ContextStats { used, total })
}

/// Connect to every enabled MCP server (cached for the app run). Servers
/// that fail to start or answer are skipped with a log line, not fatal.
fn mcp_connections(state: &AppState) -> Arc<Vec<McpConnection>> {
    let mut cached = state.harness.mcp.lock().unwrap();
    if let Some(existing) = &*cached {
        return existing.clone();
    }
    let mut out: Vec<McpConnection> = Vec::new();
    let disabled = state.config.lock().unwrap().mcp_disabled.clone();
    if let Ok(cfg) = crate::mcp::load() {
        let cfg = crate::mcp::filter_disabled(&cfg, &disabled);
        for (name, server) in cfg.servers {
            let spawn = harness::mcp::McpSession::start(
                &server.command,
                &server.args,
                &server.env,
                server.cwd.as_deref(),
                server.timeout_ms,
            );
            let session = match spawn {
                Ok(s) => Arc::new(Mutex::new(s)),
                Err(e) => {
                    log::warn!("MCP server '{}' failed to start: {e:#}", name);
                    continue;
                }
            };
            let tools = match session.lock().unwrap().list_tools() {
                Ok(t) => t,
                Err(e) => {
                    log::warn!("MCP server '{}' failed tools/list: {e:#}", name);
                    continue;
                }
            };
            log::info!("MCP server '{}' connected: {} tools", name, tools.len());
            out.push(McpConnection { server: name, session, tools });
        }
    }
    let arc = Arc::new(out);
    *cached = Some(arc.clone());
    arc
}

/// Invalidate cached MCP connections (called when mcp.json is saved).
pub fn invalidate_mcp(state: &AppState) {
    // Dropping the sessions kills the child processes.
    *state.harness.mcp.lock().unwrap() = None;
}

/// Skill roots: global ({data_dir}/catapult/skills) + project (.catapult/skills).
fn skill_roots(root: &std::path::Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(data) = dirs::data_dir() {
        roots.push(data.join("catapult").join("skills"));
    }
    roots.push(root.join(".catapult").join("skills"));
    roots
}

/// Build the tool registry for a run: native sandboxed tools + the skill tool
/// (when skills are discovered) + namespaced MCP tools (when servers connect).
fn build_registry(
    jail: Arc<PathJail>,
    state: &AppState,
    project_root_dir: &std::path::Path,
) -> (ToolRegistry, Vec<harness::skills::Skill>) {
    let mut registry = ToolRegistry::project_tools(jail);
    let skills = harness::skills::discover(&skill_roots(project_root_dir));
    if !skills.is_empty() {
        registry = registry.add(Arc::new(harness::skills::SkillTool::new(skills.clone())));
    }
    for conn in mcp_connections(state).iter() {
        for info in &conn.tools {
            registry = registry.add(Arc::new(harness::mcp::McpTool {
                server: conn.server.clone(),
                tool_name: info.name.clone(),
                session: conn.session.clone(),
                desc: info.description.clone(),
                schema: info.schema.clone(),
            }));
        }
    }
    (registry, skills)
}

/// One connected MCP server and the tools it exposes.
pub struct McpConnection {
    pub server: String,
    pub session: Arc<Mutex<harness::mcp::McpSession>>,
    pub tools: Vec<harness::mcp::McpToolInfo>,
}

/// Approval gate implementation: emits `harness_approval` to the UI and parks
/// on the pending oneshot until `harness_agent_decide` resolves it.
struct UiGate {
    app: AppHandle,
    runtime: Arc<HarnessRuntime>,
}

impl ApprovalGate for UiGate {
    fn decide(
        &self,
        req: ApprovalRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Approved> + Send>> {
        let app = self.app.clone();
        let runtime = self.runtime.clone();
        Box::pin(async move {
            let (tx, rx) = tokio::sync::oneshot::channel();
            {
                let mut pending = runtime.pending.lock().unwrap();
                if let Some(old) = pending.take() {
                    let _ = old.send(Approved::Denied); // superseded
                }
                *pending = Some(tx);
            }
            let payload = serde_json::json!({
                "type": "approval_required",
                "tool": req.key.tool,
                "command": req.key.command,
                "args": req.args_pretty,
            });
            let _ = app.emit("harness_approval", payload);
            rx.await.unwrap_or(Approved::Denied)
        })
    }
}

/// The path of the currently active chat project (or None).
fn active_project_path(config: &crate::config::AppConfig) -> Option<String> {
    let id = config.harness_active_project.as_ref()?;
    config
        .harness_projects
        .iter()
        .find(|p| &p.id == id)
        .map(|p| p.path.clone())
}

/// Resolve the sandboxed project root: the active chat project is required.
/// Chatting without a working directory is refused (the agent needs a jail).
fn project_root(state: &AppState) -> Result<PathBuf, String> {
    let active = {
        let c = state.config.lock().unwrap();
        active_project_path(&c)
    };
    let Some(dir) = active else {
        return Err(
            "Select a project (working directory) in the Chat sidebar to start chatting."
                .to_string(),
        );
    };
    let dir = PathBuf::from(dir);
    if !dir.is_dir() {
        return Err(
            "The active project's folder is missing — pick another project in the Chat sidebar."
                .to_string(),
        );
    }
    Ok(dir)
}

fn port_or_err(state: &AppState) -> Result<u16, String> {
    let s = state.server.lock().unwrap();
    match &s.status {
        crate::server::ServerStatus::Running { port, .. } => Ok(*port),
        _ => Err("Server is not running".to_string()),
    }
}

fn send_event(on_event: &Channel<String>, ev: impl serde::Serialize) {
    if let Ok(json) = serde_json::to_string(&ev) {
        let _ = on_event.send(json);
    }
}

/// Append a line to the Server Logs panel (same cap policy as the server
/// reader: 500 lines, drain 100). Harness notices live here now — they no
/// longer render as transcript cards in chat.
fn push_server_log(state: &AppState, line: &str) {
    let mut s = state.server.lock().unwrap();
    s.log_lines.push(format!("[catapult] {}", line));
    if s.log_lines.len() > 500 {
        s.log_lines.drain(0..100);
    }
}

/// Is the managed server running in router mode? (Run page: no single model.)
fn is_router_mode(state: &AppState) -> bool {
    let s = state.server.lock().unwrap();
    s.config
        .as_ref()
        .map(|c| c.model_path.is_empty())
        .unwrap_or(false)
}

/// Model-role resolution. Only effective in router mode: regenerate the
/// models-preset when roles are set, force the router to reload it, map role
/// paths to registered model ids, and load them on demand. Returns
/// (orchestrator id, worker id, VRAM notice, loading notice). The loading
/// notice is returned (not emitted here) so the caller can show it every run;
/// the VRAM notice is one-shot per app run.
async fn resolve_roles(
    state: &AppState,
    client: &LlmClient,
) -> Result<(Option<String>, Option<String>, Option<String>, Option<String>), String> {
    let roles = state.config.lock().unwrap().harness_roles.clone();
    if roles.orchestrator.is_none() && roles.worker.is_none() {
        return Ok((None, None, None, None));
    }
    if !is_router_mode(state) {
        return Err(
            "Model roles require router mode: launch with no single model selected (pin models on the Run page) and start again."
                .to_string(),
        );
    }

    // A role path that no longer exists on disk can never register — fail
    // with the concrete path instead of the generic restart hint.
    if let Some(p) = roles.orchestrator.as_deref() {
        if !std::path::Path::new(p).is_file() {
            return Err(format!(
                "Orchestrator model file not found: {p} — pick another model in Settings → Chat"
            ));
        }
    }

    // Regenerate the preset so role models are registered, then reload. The
    // registry keeps every installed model (same as server start) so a reload
    // never evicts models the user loaded via the WebUI.
    let dir = dirs::data_dir()
        .ok_or("Cannot find data directory")?
        .join("catapult");
    let worker_path = roles.worker.clone();
    let app_config = state.config.lock().unwrap().clone();
    let mut paths: Vec<String> = app_config.router_models.clone();
    if let Ok(installed) = crate::models::list_installed_models(&app_config) {
        paths.extend(installed.iter().map(|m| m.path.to_string_lossy().to_string()));
    }
    paths.extend(roles.orchestrator.clone());
    paths.extend(worker_path.clone());
    let _preset = crate::server::write_router_preset_paths(&dir, &paths).map_err(|e| e.to_string())?;
    client.router_reload().await.map_err(|e| e.to_string())?;

    // Map role paths → registered ids (section name = file stem). The reload
    // applies asynchronously, so poll briefly for the expected id instead of
    // querying once and failing on a stale list.
    let stem = |p: &str| {
        std::path::Path::new(p)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string()
    };
    let expected_orch = roles.orchestrator.as_deref().map(stem);
    let expected_worker = worker_path.as_deref().map(stem);
    let mut models = client.router_models().await.map_err(|e| e.to_string())?;
    for _ in 0..10 {
        let have_orch = expected_orch.as_ref().map_or(true, |want| {
            models.iter().any(|m| &m.id == want)
        });
        let have_worker = expected_worker.as_ref().map_or(true, |want| {
            models.iter().any(|m| &m.id == want)
        });
        if have_orch && have_worker {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        models = client.router_models().await.map_err(|e| e.to_string())?;
    }
    let find_id = |wanted: Option<&str>| -> Option<String> {
        let s = stem(wanted?);
        models.iter().find(|m| m.id == s).map(|m| m.id.clone())
    };
    let orchestrator_id = find_id(roles.orchestrator.as_deref());
    let worker_id = worker_path.as_deref().and_then(|w| find_id(Some(w)));

    if roles.orchestrator.is_some() && orchestrator_id.is_none() {
        return Err(
            "Orchestrator model is not in the router registry even after reload — restart the server"
                .to_string(),
        );
    }

    // Load the orchestrator eagerly. The worker loads lazily when the first
    // subagent actually needs it — loading both up front on a VRAM-tight
    // machine makes every request crawl (or thrash the router's LRU).
    let mut loading_notice: Option<String> = None;
    if let Some(id) = &orchestrator_id {
        let status = models.iter().find(|m| &m.id == id).map(|m| m.status.as_str());
        log::info!("harness roles: orchestrator='{id}' (router status: {})", status.unwrap_or("unknown"));
        if status != Some("loaded") {
            log::info!("harness roles: requesting load of '{id}'");
            let _ = client.router_load(id).await;
            loading_notice = Some(format!("Loading model '{id}'…"));
        }
    }
    if let Some(id) = &worker_id {
        log::info!("harness roles: worker '{id}' registered; loads lazily on first subagent use");
    }

    // ── VRAM feasibility notice (heuristic: file size ≈ fully-offloaded VRAM) ──
    let mut notice: Option<String> = None;
    if let Some(sys) = crate::hardware::get_system_info().ok() {
        let vram_mb: u64 = sys.gpus.iter().map(|g| g.vram_mb).sum();
        let mut bytes: u64 = 0;
        let measure: Vec<&String> = [&roles.orchestrator, &roles.worker]
            .into_iter()
            .flatten()
            .collect();
        for path in measure {
            if let Ok(meta) = std::fs::metadata(path) {
                bytes += meta.len();
            }
        }
        let need_gb = bytes / (1024 * 1024 * 1024);
        if vram_mb > 0 && (need_gb * 1024) > vram_mb {
            notice = Some(format!(
                "Selected role models (~{need_gb} GB) exceed VRAM ({:.1} GB) — the router may unload a model while switching roles.",
                vram_mb as f64 / 1024.0
            ));
        }
    }

    Ok((orchestrator_id, worker_id, notice, loading_notice))
}

/// What the Chat empty state shows: every tool the agent may use, with its
/// approval mode (read-only tools run automatically; mutating ones are gated
/// by the permission engine).
#[derive(Debug, Serialize)]
pub struct ToolListing {
    pub name: String,
    pub description: String,
    pub approval: String,
}

#[tauri::command]
pub async fn harness_agent_tools(state: State<'_, AppState>) -> Result<Vec<ToolListing>, String> {
    let root = project_root(&state)?;
    let jail = Arc::new(PathJail::new(&root, &[], &[]).map_err(|e| e.to_string())?);
    let (registry, _) = build_registry(jail, &state, &root);
    Ok(registry
        .names()
        .iter()
        .filter_map(|n| registry.get(n))
        .map(|t| ToolListing {
            name: t.name(),
            description: t.description(),
            approval: match t.approval_key(&serde_json::json!({})) {
                None => "auto".to_string(),
                Some(_) => "approval required".to_string(),
            },
        })
        .collect())
}

#[tauri::command]
pub async fn set_harness_max_turns(
    orchestrator: u32,
    subagent: u32,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut config = state.config.lock().unwrap();
    config.harness_max_turns = orchestrator.clamp(1, 500);
    config.harness_subagent_max_turns = subagent.clamp(1, 200);
    config.save().map_err(|e| e.to_string())
}

/// Assign harness model roles (paths of installed models, or None for the
/// server default). Takes effect when a run starts in router mode.
#[tauri::command]
pub async fn set_harness_roles(
    orchestrator: Option<String>,
    worker: Option<String>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    {
        let mut config = state.config.lock().unwrap();
        config.harness_roles.orchestrator = orchestrator.filter(|p| !p.trim().is_empty());
        config.harness_roles.worker = worker.filter(|p| !p.trim().is_empty());
        config.save().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn harness_agent_send(
    message: String,
    reasoning_effort: Option<String>,
    attachments: Option<Vec<crate::attachments::Attachment>>,
    on_event: Channel<String>,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<RunResult, String> {
    if state
        .harness
        .running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("An agent run is already in progress".to_string());
    }
    let _running_guard = RunningGuard(state.harness.clone());

    load_session(&state);
    let root = project_root(&state)?;
    let jail = Arc::new(PathJail::new(&root, &[], &[]).map_err(|e| e.to_string())?);
    let port = port_or_err(&state)?;
    let client = LlmClient::new(format!("http://127.0.0.1:{port}"));

    // Tools: native sandboxed + skills + MCP (skills also extend the prompt).
    let (registry, skills) = build_registry(jail.clone(), &state, &root);

    // ── Model roles (Phase 3) ──
    // In router mode the chat request REQUIRES a model name ("Server default"
    // means: the router's running/registered model, not an omitted field).
    let (mut orchestrator_id, worker_id, vram_notice, loading_notice) =
        resolve_roles(&state, &client).await?;
    // Take the history out (never hold the mutex across the async loop).
    let mut history = std::mem::take(&mut *state.harness.history.lock().unwrap());
    if !history.iter().any(|m| m.role == "system") {
        // Byte-stable per project → good prefix-cache behavior.
        history.insert(
            0,
            ChatMessage::system(format!(
                "{SYSTEM_PROMPT}\n\nProject directory: {}{}",
                root.display(),
                harness::skills::system_prompt_listing(&skills)
            )),
        );
    }
    history.push(crate::attachments::build_user_message(message, attachments));

    // Configurable turn budgets (clamped by the setter, clamped again here).
    let (max_turns, subagent_max_turns) = {
        let c = state.config.lock().unwrap();
        (c.harness_max_turns.clamp(1, 500), c.harness_subagent_max_turns.clamp(1, 200))
    };

    state.harness_abort.store(false, Ordering::SeqCst);
    let abort = state.harness_abort.clone();
    let should_stop = Arc::new(move || abort.load(Ordering::SeqCst)) as std::sync::Arc<dyn Fn() -> bool + Send + Sync>;

    let gate: Arc<dyn ApprovalGate> = Arc::new(UiGate {
        app: app.clone(),
        runtime: state.harness.clone(),
    });

    let mut sink = |ev: StreamEvent| {
        if let StreamEvent::Notice { text } = &ev {
            push_server_log(&state, text);
        }
        send_event(&on_event, ev);
    };
    let mut event_sink = |ev: AgentEvent| {
        if let AgentEvent::Notice { text } = &ev {
            push_server_log(&state, text);
        }
        send_event(&on_event, ev);
    };
    // Loading state shows on every run that needs it; the VRAM warning is
    // one-shot per app run.
    if let Some(text) = loading_notice {
        push_server_log(&state, &text);
        send_event(&on_event, AgentEvent::Notice { text });
    }
    if let Some(text) = vram_notice {
        if !state.harness.notice_shown.swap(true, Ordering::SeqCst) {
            push_server_log(&state, &text);
            send_event(&on_event, AgentEvent::Notice { text });
        }
    }

    // In router mode the chat request REQUIRES a model name ("Server default"
    // means: the router's running/registered model, not an omitted field).
    if orchestrator_id.is_none() && is_router_mode(&state) {
        if let Ok(models) = client.router_models().await {
            orchestrator_id = models
                .iter()
                .find(|m| m.status == "loaded")
                .or_else(|| models.first())
                .map(|m| m.id.clone());
            if orchestrator_id.is_none() {
                return Err(
                    "No models are registered with the router — pin models on the Run page and restart the server."
                        .to_string(),
                );
            }
        }
    }

    let run = AgentRun {
        client: &client,
        registry: Arc::new(registry),
        engine: state.harness.engine.clone(),
        model: orchestrator_id.clone(),
        reasoning_effort: reasoning_effort.filter(|e| !e.is_empty() && e != "default"),
        max_turns: max_turns as usize,
        subagents: Some(harness::agent::Subagents {
            jail,
            max_turns: subagent_max_turns as usize,
            model: worker_id,
        }),
    };

    let result = run
        .run(
            &mut history,
            should_stop,
            gate,
            &mut sink,
            &mut event_sink,
        )
        .await;

    // Persist the transcript for the session (also on abort/error, so the
    // conversation stays inspectable and resumes after a restart).
    *state.harness.history.lock().unwrap() = history;
    save_session(&state);

    // Resolve the display model name: the role model id, else the loaded one.
    let model = match orchestrator_id.clone() {
        Some(id) => Some(id),
        None => client.router_models().await.ok().and_then(|m| m.first().map(|x| x.id.clone())),
    };
    match result {
        Ok(outcome) => {
            *state.harness.last_prompt_tokens.lock().unwrap() = outcome.prompt_tokens;
            Ok(RunResult {
                text: outcome.text,
                model,
                tokens_per_sec: outcome.tokens_per_sec,
                gen_tokens: outcome.gen_tokens,
                prompt_tokens: outcome.prompt_tokens,
                elapsed_ms: outcome.elapsed_ms,
            })
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Run-result payload for the Chat UI (model + speed under each response).
#[derive(Debug, Serialize)]
pub struct RunResult {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_per_sec: Option<f64>,
    pub gen_tokens: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u64>,
    pub elapsed_ms: u64,
}

/// Capabilities of the active model (GGUF metadata): vision (mmproj) and
/// reasoning — shown as badges next to the chat input.
#[derive(Debug, Serialize)]
pub struct HarnessCapabilities {
    pub vision: bool,
    pub reasoning: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_length: Option<u64>,
}

/// Path of the model chat should target: orchestrator role, else the single
/// loaded model. `None` when neither is set (router mode with no registered
/// default).
fn active_model_path(state: &AppState) -> Option<String> {
    let config = state.config.lock().unwrap();
    if let Some(role) = config.harness_roles.orchestrator.clone() {
        return Some(role);
    }
    let server = state.server.lock().unwrap();
    server
        .config
        .as_ref()
        .filter(|cfg| !cfg.model_path.is_empty())
        .map(|cfg| cfg.model_path.clone())
}

#[derive(Debug, Serialize)]
pub struct ReasoningOptions {
    /// Whether the active model reasons at all (tag, template, or effort knobs).
    pub supported: bool,
    /// `reasoning_effort` ids the chat template accepts, in template order.
    /// Empty when no levels could be parsed — the UI must then not offer any.
    pub levels: Vec<String>,
}

#[tauri::command]
pub async fn harness_reasoning_options(state: State<'_, AppState>) -> Result<ReasoningOptions, String> {
    let Some(path) = active_model_path(&state) else {
        return Ok(ReasoningOptions { supported: false, levels: vec![] });
    };
    let (supported, levels) = crate::models::read_model_metadata(std::path::Path::new(&path))
        .map(|m| crate::models::reasoning_support(&m))
        .unwrap_or((false, vec![]));
    Ok(ReasoningOptions { supported, levels })
}

#[tauri::command]
pub async fn harness_agent_capabilities(state: State<'_, AppState>) -> Result<HarnessCapabilities, String> {
    // Prefer the orchestrator role model; fall back to the single loaded model.
    let path = active_model_path(&state);
    let Some(path) = path else {
        return Ok(HarnessCapabilities { vision: false, reasoning: false, context_length: None });
    };
    let meta = crate::models::read_model_metadata(std::path::Path::new(&path));
    let has_cap = |want: &str| {
        meta.as_ref()
            .map(|m| m.capabilities.iter().any(|c| c.eq_ignore_ascii_case(want)))
            .unwrap_or(false)
    };
    // Same template-aware detection as the reason-command above: a template
    // driving reasoning behavior counts even without a capability string.
    let reasoning = has_cap("reasoning")
        || meta
            .as_ref()
            .map(|m| crate::models::reasoning_support(m).0)
            .unwrap_or(false);
    Ok(HarnessCapabilities {
        vision: has_cap("vision"),
        reasoning,
        context_length: meta.as_ref().and_then(|m| m.context_length),
    })
}

struct RunningGuard(Arc<HarnessRuntime>);

impl Drop for RunningGuard {
    fn drop(&mut self) {
        self.0.running.store(false, Ordering::SeqCst);
    }
}

#[tauri::command]
pub async fn harness_agent_abort(state: State<'_, AppState>) -> Result<(), String> {
    state.harness_abort.store(true, Ordering::SeqCst);
    // If parked on an approval, unblock with a denial; the loop's next
    // should_stop check ends the run.
    if let Some(tx) = state.harness.pending.lock().unwrap().take() {
        let _ = tx.send(Approved::Denied);
    }
    Ok(())
}

/// User decision for the pending approval prompt. `grant` = None denies;
/// "once" allows a single call; "session" grants the tool (or command head)
/// for 30 minutes.
#[tauri::command]
pub async fn harness_agent_decide(
    grant: Option<String>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let sender = state.harness.pending.lock().unwrap().take();
    let scope = match grant.as_deref() {
        Some("once") => Approved::Once,
        Some("session") => Approved::Session,
        _ => Approved::Denied,
    };
    if let Some(tx) = sender {
        let _ = tx.send(scope);
    }
    Ok(())
}

#[tauri::command]
pub async fn harness_agent_reset(state: State<'_, AppState>) -> Result<(), String> {
    state.harness.history.lock().unwrap().clear();
    *state.harness.session_id.lock().unwrap() = None;
    let mut pending = state.harness.pending.lock().unwrap();
    if let Some(tx) = pending.take() {
        let _ = tx.send(Approved::Denied);
    }
    Ok(())
}

/// The current runtime transcript (frontend rebuilds its view after a session
/// load or project switch). Loading the persisted session first so chats are
/// consultable without the server running.
#[tauri::command]
pub async fn harness_agent_history(state: State<'_, AppState>) -> Result<Vec<ChatMessage>, String> {
    load_session(&state);
    Ok(state.harness.history.lock().unwrap().clone())
}

// ── Sessions ────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn harness_sessions_list() -> Result<Vec<SessionInfo>, String> {
    let Some(dir) = session_dir() else { return Ok(vec![]) };
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if let Some(s) = read_session_file(&path) {
                out.push(SessionInfo {
                    id: s.id,
                    title: s.title,
                    updated: s.updated,
                    project: s.project,
                });
            }
        }
    }
    out.sort_by(|a, b| b.updated.cmp(&a.updated));
    Ok(out)
}

#[tauri::command]
pub async fn harness_session_load(id: String, state: State<'_, AppState>) -> Result<(), String> {
    let Some(path) = session_file(&id) else {
        return Err("Unknown session".to_string());
    };
    let s = read_session_file(&path).ok_or("Session file is corrupt")?;
    *state.harness.history.lock().unwrap() = s.messages;
    *state.harness.session_id.lock().unwrap() = Some(s.id);
    Ok(())
}

#[tauri::command]
pub async fn harness_session_delete(id: String, state: State<'_, AppState>) -> Result<(), String> {
    if state.harness.session_id.lock().unwrap().as_deref() == Some(id.as_str()) {
        // Deleting the open session also starts a fresh one.
        state.harness.history.lock().unwrap().clear();
        *state.harness.session_id.lock().unwrap() = None;
    }
    if let Some(path) = session_file(&id) {
        std::fs::remove_file(path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Rewind: drop the most recent user turn (message + everything after it).
#[tauri::command]
pub async fn harness_agent_rewind(state: State<'_, AppState>) -> Result<(), String> {
    let mut history = state.harness.history.lock().unwrap();
    let Some(idx) = history.iter().rposition(|m| m.role == "user") else {
        return Ok(());
    };
    history.truncate(idx);
    drop(history);
    save_session(&state);
    Ok(())
}

// ── Git worktrees (parallel agent branches) ─────────────────────────────────

#[derive(Debug, Serialize, Clone)]
pub struct WorktreeInfo {
    pub path: String,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub bare: bool,
    pub main: bool,
}

fn valid_branch_name(branch: &str) -> bool {
    !branch.trim().is_empty()
        && branch
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_/+.".contains(c))
}

#[tauri::command]
pub async fn harness_git_is_repo(root: String) -> Result<bool, String> {
    Ok(harness::git::is_repo(std::path::Path::new(&root)))
}

#[tauri::command]
pub async fn harness_worktree_list(root: String) -> Result<Vec<WorktreeInfo>, String> {
    let entries = harness::git::list(std::path::Path::new(&root)).map_err(|e| e.to_string())?;
    Ok(entries
        .into_iter()
        .map(|w| WorktreeInfo {
            path: w.path,
            branch: w.branch,
            head: w.head,
            bare: w.bare,
            main: w.main,
        })
        .collect())
}

/// Create a worktree for `branch` as a *sibling* of the repo root
/// (`<repo-parent>/<repo-name>-<branch>`). Returns the worktree path.
#[tauri::command]
pub async fn harness_worktree_add(root: String, branch: String) -> Result<String, String> {
    let branch = branch.trim();
    if !valid_branch_name(branch) {
        return Err("Branch name may only contain letters, numbers, - _ / + .".to_string());
    }
    let repo = std::path::Path::new(&root);
    let parent = repo.parent().ok_or("Cannot determine repo parent")?;
    let repo_name = repo
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("Cannot determine repo name")?;
    let safe_branch = branch.replace(['/', '\\'], "-");
    let path = parent.join(format!("{}-{}", repo_name, safe_branch));
    if path.exists() {
        return Err(format!("{} already exists", path.display()));
    }
    harness::git::add(repo, &path, Some(branch)).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn harness_worktree_remove(
    root: String,
    path: String,
    force: bool,
) -> Result<(), String> {
    harness::git::remove(
        std::path::Path::new(&root),
        &path,
        force,
    )
    .map_err(|e| e.to_string())
}

// ── Projects (contained working directories) ────────────────────────────────

#[tauri::command]
pub async fn harness_project_add(path: String, state: State<'_, AppState>) -> Result<(), String> {
    let path = path.trim().to_string();
    if path.is_empty() {
        return Err("Empty path".to_string());
    }
    let abs = std::fs::canonicalize(&path).map_err(|e| e.to_string())?;
    if !abs.is_dir() {
        return Err("Not a directory".to_string());
    }
    let name = abs
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("project")
        .to_string();
    let id = abs.to_string_lossy().to_lowercase().replace('\\', "/").replace(':', "");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut config = state.config.lock().unwrap();
    if config.harness_projects.iter().any(|p| p.id == id) {
        config.harness_active_project = Some(id);
    } else {
        config.harness_projects.push(crate::config::HarnessProject {
            id: id.clone(),
            name,
            path: abs.to_string_lossy().to_string(),
            created: now,
        });
        config.harness_active_project = Some(id);
    }
    config.save().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn harness_project_remove(id: String, state: State<'_, AppState>) -> Result<(), String> {
    let mut config = state.config.lock().unwrap();
    config.harness_projects.retain(|p| p.id != id);
    if config.harness_active_project.as_deref() == Some(id.as_str()) {
        config.harness_active_project = None;
    }
    config.save().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn harness_project_active(id: Option<String>, state: State<'_, AppState>) -> Result<(), String> {
    let mut config = state.config.lock().unwrap();
    if let Some(id) = &id {
        if !config.harness_projects.iter().any(|p| &p.id == id) {
            return Err("Unknown project".to_string());
        }
    }
    config.harness_active_project = id;
    config.save().map_err(|e| e.to_string())
}
