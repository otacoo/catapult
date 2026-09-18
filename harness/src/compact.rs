//! Context compaction: when the transcript outgrows the context window, the
//! oldest turns are summarized by the model itself and replaced with one
//! summary message. Cuts always land on user boundaries so no tool
//! call/result pair is ever split.

use anyhow::{bail, Result};
use serde_json::Value;

use crate::agent::AgentEvent;
use crate::client::{ChatMessage, LlmClient, StreamEvent};

/// Compact once the estimate passes this fraction of the context limit.
pub const TRIGGER_FRACTION: f64 = 0.8;
/// Recent messages kept verbatim (more when needed to hit a user boundary).
pub const KEEP_RECENT: usize = 12;
/// How far back to scan for a user boundary before giving up.
const MAX_SCAN: usize = 48;
/// Transcript text fed to the summarizer (oldest first; ~4 chars/token).
const SUMMARIZE_CAP_CHARS: usize = 60_000;
/// Per-message cap inside the summarized transcript.
const MESSAGE_CAP_CHARS: usize = 2_000;
/// Summary message marker; the UI renders these distinctly.
pub const SUMMARY_MARKER: &str = "[Compacted context";

/// Chars/4 estimate, consistent with the context ring's fallback.
pub fn estimate_tokens(history: &[ChatMessage]) -> u64 {
    let chars: usize = history
        .iter()
        .filter_map(|m| m.content.as_ref())
        .map(|c| match c {
            Value::String(s) => s.len(),
            other => other.to_string().len(),
        })
        .sum();
    (chars / 4) as u64
}

pub fn is_summary(m: &ChatMessage) -> bool {
    m.content
        .as_ref()
        .and_then(|c| c.as_str())
        .map(|s| s.starts_with(SUMMARY_MARKER))
        .unwrap_or(false)
}

/// Cut index (first kept message) honoring user boundaries. `None` when there
/// is nothing worth removing.
pub fn plan_cut(history: &[ChatMessage], keep: usize) -> Option<usize> {
    let protected = if history.first().map(|m| m.role.as_str()) == Some("system") {
        1
    } else {
        0
    };
    if history.len() <= protected + keep + 1 {
        return None;
    }
    // Ideal cut keeps the last `keep` messages; scan back for a user boundary.
    let ideal = history.len().saturating_sub(keep);
    let floor = ideal.saturating_sub(MAX_SCAN).max(protected + 1);
    for cut in (floor..=ideal).rev() {
        if history[cut].role == "user" && !is_summary(&history[cut]) {
            return Some(cut);
        }
    }
    // Degenerate tail (no user turn in range): cut at the ideal point anyway
    // but drop leading non-user messages so no orphan tool pair survives.
    let mut cut = ideal.max(protected + 1);
    while cut < history.len() && history[cut].role != "user" {
        cut += 1;
    }
    (cut < history.len()).then_some(cut)
}

fn message_text(m: &ChatMessage) -> String {
    match m.content.as_ref() {
        None => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| {
                if p.get("type").and_then(|t| t.as_str()) == Some("text") {
                    p.get("text").and_then(|t| t.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(other) => other.to_string(),
    }
}

fn render_for_summary(m: &ChatMessage) -> Option<String> {
    let mut text = message_text(m);
    if text.chars().count() > MESSAGE_CAP_CHARS {
        text = text.chars().take(MESSAGE_CAP_CHARS).collect();
        text.push_str("\n[…]");
    }
    if text.trim().is_empty() && m.tool_calls.as_ref().map(|c| c.is_empty()).unwrap_or(true) {
        return None;
    }
    let line = match m.role.as_str() {
        "user" => format!("user: {text}"),
        "assistant" => match m.tool_calls.as_ref().filter(|c| !c.is_empty()) {
            Some(calls) => {
                let names: Vec<String> = calls
                    .iter()
                    .map(|c| {
                        let args = c.function.arguments.chars().take(120).collect::<String>();
                        format!("{}({})", c.function.name, args)
                    })
                    .collect();
                if text.trim().is_empty() {
                    format!("assistant tool calls: {}", names.join(", "))
                } else {
                    format!("assistant: {text} [tool calls: {}]", names.join(", "))
                }
            }
            None => format!("assistant: {text}"),
        },
        "tool" => format!("tool result: {text}"),
        _ => return None,
    };
    Some(line)
}

pub struct CompactionInfo {
    /// First kept message index (for footer-meta reindexing).
    pub cut: usize,
    /// Messages removed (replaced by one summary).
    pub removed: usize,
}

/// Summarize `history[1..cut]` and splice in one summary message. Returns
/// `None` when under the trigger; errors when compaction cannot help.
pub async fn compact_history(
    client: &LlmClient,
    model: Option<&str>,
    history: &mut Vec<ChatMessage>,
    context_limit: u64,
    should_stop: &(dyn Fn() -> bool + Send + Sync),
    on_event: &mut (dyn FnMut(AgentEvent) + Send),
) -> Result<Option<CompactionInfo>> {
    if context_limit == 0 || estimate_tokens(history) < (context_limit as f64 * TRIGGER_FRACTION) as u64 {
        return Ok(None);
    }
    let Some(cut) = plan_cut(history, KEEP_RECENT) else {
        bail!("Conversation is too long to continue and has no compactable turns — start a new chat (/new)");
    };
    // The kept tail must itself fit with room for the summary and the reply.
    let tail_estimate = estimate_tokens(&history[cut..]);
    if tail_estimate > (context_limit as f64 * 0.9) as u64 {
        bail!("Conversation is too long even after compaction — start a new chat (/new)");
    }
    let mut rendered: Vec<String> = history[1..cut].iter().filter_map(render_for_summary).collect();
    let mut transcript = rendered.join("\n\n");
    if transcript.chars().count() > SUMMARIZE_CAP_CHARS {
        transcript = transcript.chars().take(SUMMARIZE_CAP_CHARS).collect();
    }
    if transcript.trim().is_empty() {
        bail!("Conversation is too long even after compaction — start a new chat (/new)");
    }
    rendered.clear();

    let mut summary = String::new();
    client
        .chat_stream(
            model,
            &[
                ChatMessage::system(
                    "Summarize this conversation for continuation in a fresh context window. \
                    Preserve: the user's goals, key decisions and why, files created or changed, \
                    pending tasks, and any durable facts or preferences. Be concise; use bullets.",
                ),
                ChatMessage::user(transcript),
            ],
            None,
            None,
            || should_stop(),
            &mut |ev| {
                if let StreamEvent::Content { text } = ev {
                    summary.push_str(&text);
                }
            },
        )
        .await?;
    if should_stop() {
        bail!("aborted");
    }
    let summary = summary.trim().to_string();
    if summary.is_empty() {
        bail!("Compaction produced an empty summary — start a new chat (/new)");
    }
    let removed = cut - 1;
    history.splice(
        1..cut,
        [ChatMessage::user(format!(
            "{SUMMARY_MARKER} — summary of {removed} earlier messages]\n\n{summary}"
        ))],
    );
    on_event(AgentEvent::Compacted { removed });
    Ok(Some(CompactionInfo { cut, removed }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> ChatMessage {
        ChatMessage::user(text.to_string())
    }

    fn history(n_turns: usize) -> Vec<ChatMessage> {
        let mut h = vec![ChatMessage::system("sys")];
        for i in 0..n_turns {
            h.push(user(&format!("question {i}")));
            h.push(ChatMessage::assistant(format!("answer {i}")));
        }
        h
    }

    #[test]
    fn estimate_counts_chars_over_four() {
        let h = vec![ChatMessage::system("12345678")];
        assert_eq!(estimate_tokens(&h), 2);
        assert_eq!(estimate_tokens(&[]), 0);
    }

    #[test]
    fn plan_cut_keeps_recent_on_user_boundary() {
        // 1 system + 20 turns (40 msgs); keep 12 → cut at a user message.
        let h = history(20);
        let cut = plan_cut(&h, 12).unwrap();
        assert_eq!(h[cut].role, "user");
        assert!(h.len() - cut >= 12, "keeps at least the recent window");
        assert!(h.len() - cut <= 13, "lands on the nearest boundary");
    }

    #[test]
    fn plan_cut_nothing_to_remove() {
        let h = history(2);
        assert_eq!(plan_cut(&h, 12), None);
    }

    #[test]
    fn plan_cut_skips_existing_summaries() {
        let mut h = history(20);
        // A summary sitting exactly at the ideal cut must not become the cut.
        let ideal = h.len() - 12;
        h[ideal] = ChatMessage::user(format!("{SUMMARY_MARKER} — old]\n\nstuff"));
        let cut = plan_cut(&h, 12).unwrap();
        assert_ne!(cut, ideal);
        assert_eq!(h[cut].role, "user");
        assert!(!is_summary(&h[cut]));
    }

    #[test]
    fn render_skips_system_and_caps_tool_results() {
        let long = "x".repeat(5000);
        let m = ChatMessage {
            role: "tool".into(),
            content: Some(Value::String(long)),
            tool_calls: None,
            tool_call_id: Some("c1".into()),
        };
        let line = render_for_summary(&m).unwrap();
        assert!(line.starts_with("tool result: "));
        assert!(line.chars().count() < MESSAGE_CAP_CHARS + 100);
        assert!(render_for_summary(&ChatMessage::system("s")).is_none());
    }
}
