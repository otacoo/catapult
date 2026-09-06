// ── Harness commands ────────────────────────────────────────────────────────
//
// Phase 0: native streaming chat against the running llama-server. Events are
// pushed to the frontend via a Tauri `Channel` (typed callback, survives the
// invoke promise). The agent loop / tools land in Phase 1.

use tauri::ipc::Channel;
use tauri::State;

use crate::AppState;
use harness::client::{LlmClient, StreamEvent};

#[tauri::command]
pub async fn harness_chat_send(
    messages: Vec<harness::client::ChatMessage>,
    on_event: Channel<String>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let port = {
        let s = state.server.lock().unwrap();
        match &s.status {
            crate::server::ServerStatus::Running { port, .. } => *port,
            _ => return Err("Server is not running".to_string()),
        }
    };

    // Reset the cooperative abort flag for this run.
    state
        .harness_abort
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let abort = state.harness_abort.clone();

    let client = LlmClient::new(format!("http://127.0.0.1:{port}"));
    let should_stop = move || abort.load(std::sync::atomic::Ordering::SeqCst);
    let sink = |ev: StreamEvent| {
        if let Ok(json) = serde_json::to_string(&ev) {
            let _ = on_event.send(json);
        }
    };
    client
        .chat_stream(None, &messages, should_stop, sink)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn harness_chat_abort(state: State<'_, AppState>) -> Result<(), String> {
    state
        .harness_abort
        .store(true, std::sync::atomic::Ordering::SeqCst);
    Ok(())
}
