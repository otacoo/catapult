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

use serde::Serialize;
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
        }
    }
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

/// On-disk session transcript (resumed on the next app start).
fn session_path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("catapult").join("sessions").join("current.json"))
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

fn load_session(state: &AppState) {
    let rt = &state.harness;
    if rt.session_loaded.swap(true, Ordering::SeqCst) {
        return;
    }
    if let Some(path) = session_path() {
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(history) = serde_json::from_str::<Vec<ChatMessage>>(&content) {
                *rt.history.lock().unwrap() = history;
            }
        }
    }
}

fn save_session(state: &AppState) {
    if let Some(path) = session_path() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let history = state.harness.history.lock().unwrap().clone();
        if let Ok(json) = serde_json::to_string(&history) {
            let _ = std::fs::write(&path, json);
        }
    }
}

/// Resolve (and create) the sandboxed project root: the current server
/// working directory when set, else the shared Catapult workspace.
fn project_root(state: &AppState) -> Result<PathBuf, String> {
    let working = {
        let s = state.server.lock().unwrap();
        s.config
            .as_ref()
            .and_then(|c| c.working_dir.clone())
            .filter(|d| !d.trim().is_empty())
    };
    let dir = match working {
        Some(d) => PathBuf::from(d),
        None => dirs::data_dir()
            .ok_or("Cannot find data directory")?
            .join("catapult")
            .join("workspace"),
    };
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
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
/// (orchestrator id, worker id, VRAM notice).
async fn resolve_roles(
    state: &AppState,
    client: &LlmClient,
) -> Result<(Option<String>, Option<String>, Option<String>), String> {
    let roles = state.config.lock().unwrap().harness_roles.clone();
    let (Some(orch_path), _) = (roles.orchestrator.as_ref(), roles.worker.as_ref()) else {
        return Ok((None, None, None));
    };
    if !is_router_mode(state) {
        return Err(
            "Model roles require router mode: launch with no single model selected (pin models on the Run page) and start again."
                .to_string(),
        );
    }

    // Regenerate the preset so role models are registered, then reload.
    let dir = dirs::data_dir()
        .ok_or("Cannot find data directory")?
        .join("catapult");
    let worker_path = roles.worker.clone();
    let mut paths: Vec<String> = state.config.lock().unwrap().router_models.clone();
    paths.push(orch_path.clone());
    paths.extend(worker_path.clone());
    let _preset = crate::server::write_router_preset_paths(&dir, &paths).map_err(|e| e.to_string())?;
    client.router_reload().await.map_err(|e| e.to_string())?;

    // Map role paths → registered ids (section name = file stem).
    let models = client.router_models().await.map_err(|e| e.to_string())?;
    let stem = |p: &str| {
        std::path::Path::new(p)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string()
    };
    let orch_stem = stem(&orch_path);
    let find_id = |wanted: Option<&str>| -> Option<String> {
        let s = stem(wanted?);
        models.iter().find(|m| m.id == s).map(|m| m.id.clone())
    };
    let orchestrator_id = find_id(Some(&orch_stem));
    let worker_id = worker_path.as_ref().map(|w| stem(w)).and_then(|s| {
        models
            .iter()
            .find(|m| m.id.eq_ignore_ascii_case(&s))
            .map(|m| m.id.clone())
    });

    if orchestrator_id.is_none() {
        return Err(format!(
            "Orchestrator model '{}' could not be registered with the router",
            orch_stem
        ));
    }

    // Load on demand (already-running models return an error we tolerate).
    for id in [&orchestrator_id, &worker_id].into_iter().flatten() {
        if let Some(m) = models.iter().find(|m| &m.id == id) {
            if m.status != "loaded" {
                let _ = client.router_load(id).await;
            }
        }
    }

    // ── VRAM feasibility notice (heuristic: file size ≈ fully-offloaded VRAM) ──
    let mut notice: Option<String> = None;
    if let Some(sys) = crate::hardware::get_system_info().ok() {
        let vram_mb: u64 = sys.gpus.iter().map(|g| g.vram_mb).sum();
        let mut bytes: u64 = 0;
        let mut measure: Vec<&str> = vec![orch_path.as_str()];
        if let Some(w) = worker_path.as_deref() {
            measure.push(w);
        }
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

    Ok((orchestrator_id, worker_id, notice))
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
    on_event: Channel<String>,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<String, String> {
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
    let (orchestrator_id, worker_id, notice) = resolve_roles(&state, &client).await?;

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
    history.push(ChatMessage::user(message));

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

    let mut sink = |ev: StreamEvent| send_event(&on_event, ev);
    let mut event_sink = |ev: AgentEvent| send_event(&on_event, ev);
    if let Some(text) = notice {
        if !state.harness.notice_shown.swap(true, Ordering::SeqCst) {
            send_event(&on_event, AgentEvent::Notice { text });
        }
    }

    let run = AgentRun {
        client: &client,
        registry: Arc::new(registry),
        engine: state.harness.engine.clone(),
        model: orchestrator_id,
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
    result.map_err(|e| e.to_string())
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
    if let Some(path) = session_path() {
        let _ = std::fs::remove_file(path);
    }
    let mut pending = state.harness.pending.lock().unwrap();
    if let Some(tx) = pending.take() {
        let _ = tx.send(Approved::Denied);
    }
    Ok(())
}
