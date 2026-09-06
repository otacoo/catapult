// ── Harness runtime + commands ──────────────────────────────────────────────
//
// Phase 1: the agent loop (orchestrator) with sandboxed tools, running against
// the managed llama-server. State lives in `HarnessRuntime` (shared via
// AppState): message history for the current session, the permission engine,
// an in-flight marker, and the pending approval channel the loop parks on.
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
}

impl HarnessRuntime {
    pub fn new() -> Self {
        Self {
            history: Mutex::new(Vec::new()),
            engine: Arc::new(PermissionEngine::new()),
            running: std::sync::atomic::AtomicBool::new(false),
            pending: Mutex::new(None),
            session_loaded: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

/// On-disk session transcript (resumed on the next app start).
fn session_path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("catapult").join("sessions").join("current.json"))
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
    let registry = ToolRegistry::project_tools(jail);
    Ok(registry
        .names()
        .iter()
        .filter_map(|n| registry.get(n))
        .map(|t| ToolListing {
            name: t.name().to_string(),
            description: t.description().to_string(),
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

    // Take the history out (never hold the mutex across the async loop).
    let mut history = std::mem::take(&mut *state.harness.history.lock().unwrap());
    if !history.iter().any(|m| m.role == "system") {
        // Byte-stable per project → good prefix-cache behavior.
        history.insert(
            0,
            ChatMessage::system(format!("{SYSTEM_PROMPT}\n\nProject directory: {}", root.display())),
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

    let run = AgentRun {
        client: &client,
        registry: Arc::new(ToolRegistry::project_tools(jail.clone())),
        engine: state.harness.engine.clone(),
        model: None, // server's loaded model; role routing lands in Phase 3
        max_turns: max_turns as usize,
        subagents: Some(harness::agent::Subagents {
            jail,
            max_turns: subagent_max_turns as usize,
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
