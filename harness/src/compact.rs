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
/// Recent messages kept verbatim — superseded by KEEP_RECENT_TOKENS.
pub const KEEP_RECENT_TOKENS: u64 = 8_192;
/// Tokens reserved for the reply + tool schemas: the real trigger is
/// `estimate > limit - reserve`, capped for tiny windows.
pub fn reserve_tokens(context_limit: u64) -> u64 {
    16_384.min(context_limit / 2)
}
/// Tool results above this many chars are trimmed head/tail before summarizing
/// (dsh's tool-result pruner) — often relieves pressure without a summary.
pub const TOOL_RESULT_TRIM_THRESHOLD: usize = 8_192;
/// How far back to scan for a user boundary before giving up.
const MAX_SCAN: usize = 48;
/// Transcript text fed to the summarizer (oldest first; ~4 chars/token).
const SUMMARIZE_CAP_CHARS: usize = 60_000;
/// Per-message cap inside the summarized transcript.
const MESSAGE_CAP_CHARS: usize = 2_000;
/// Summary message marker; the UI renders these distinctly.
pub const SUMMARY_MARKER: &str = "[Compacted context";

/// Chars/4 estimate, consistent with the context ring's fallback. Array
/// content counts text parts only — counting the raw JSON would charge every
/// attached image's base64 payload to the transcript.
pub fn estimate_tokens(history: &[ChatMessage]) -> u64 {
    let chars: usize = history
        .iter()
        .filter_map(|m| m.content.as_ref())
        .map(|c| match c {
            Value::String(s) => s.len(),
            Value::Array(parts) => parts
                .iter()
                .filter_map(|p| {
                    if p.get("type").and_then(|t| t.as_str()) == Some("text") {
                        p.get("text").and_then(|t| t.as_str()).map(|s| s.len())
                    } else {
                        None
                    }
                })
                .sum::<usize>(),
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
/// is nothing worth removing. Token-budget walk-back: keep the newest
/// messages worth ~`keep_tokens`; light messages keep more of them, heavy
/// messages fewer — the budget is what matters.
pub fn plan_cut(history: &[ChatMessage], keep_tokens: u64) -> Option<usize> {
    let protected = if history.first().map(|m| m.role.as_str()) == Some("system") {
        1
    } else {
        0
    };
    if history.len() <= protected + 3 {
        return None;
    }
    // Walk back from the newest message until the budget is accumulated.
    // Candidates start at `protected` (the first non-system message).
    let mut cut = protected;
    let mut acc = 0u64;
    for idx in (protected..history.len()).rev() {
        cut = idx;
        acc += estimate_tokens(&history[idx..idx + 1]);
        if acc >= keep_tokens {
            break;
        }
    }
    // Boundary snap: prefer a user message at or before the walk-back point.
    let floor = cut.saturating_sub(MAX_SCAN).max(protected);
    for c in (floor..=cut).rev() {
        if history[c].role == "user" && !is_summary(&history[c]) {
            return Some(c);
        }
    }
    // Degenerate tail (no user turn in range): advance past non-user messages
    // and old summaries so neither orphans nor double-compacts.
    let mut c = cut;
    while c < history.len() && (history[c].role != "user" || is_summary(&history[c])) {
        c += 1;
    }
    (c < history.len()).then_some(c)
}

/// Trim over-budget tool results IN PLACE (head / marker / tail):
/// cheap pressure relief that may skip the summary call entirely. Returns the
/// number of trimmed results.
pub fn prune_tool_results(history: &mut [ChatMessage]) -> usize {
    let mut trimmed = 0;
    for m in history.iter_mut() {
        if m.role != "tool" {
            continue;
        }
        let Some(Value::String(text)) = m.content.as_ref() else {
            continue;
        };
        if text.chars().count() <= TOOL_RESULT_TRIM_THRESHOLD {
            continue;
        }
        let head: String = text.chars().take(TOOL_RESULT_TRIM_THRESHOLD / 2).collect();
        let tail: String = text
            .chars()
            .skip(text.chars().count() - 1_024)
            .collect();
        m.content = Some(Value::String(format!(
            "{head}\n[…middle pruned to save context; the session log has the full output…]\n{tail}"
        )));
        trimmed += 1;
    }
    trimmed
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

/// Cumulative file operations: every write/edit path mentioned in the
/// summarized span, so the summary tells the model the final file state.
fn file_ops_section(history: &[ChatMessage]) -> Option<String> {
    let mut paths: Vec<String> = Vec::new();
    for m in history {
        let Some(calls) = m.tool_calls.as_ref().filter(|c| !c.is_empty()) else {
            continue;
        };
        for call in calls {
            if !matches!(call.function.name.as_str(), "write_file" | "edit_file") {
                continue;
            }
            if let Ok(args) = serde_json::from_str::<Value>(&call.function.arguments) {
                if let Some(p) = args.get("path").and_then(|p| p.as_str()) {
                    let p = p.trim();
                    if !p.is_empty() && !paths.iter().any(|x| x == p) {
                        paths.push(p.to_string());
                    }
                }
            }
        }
    }
    if paths.is_empty() {
        return None;
    }
    Some(format!(
        "Files created or modified during this conversation:\n{}",
        paths.iter().map(|p| format!("- {p}")).collect::<Vec<_>>().join("\n")
    ))
}

pub struct CompactionInfo {
    /// First kept message index (for footer-meta reindexing). `None` when
    /// only tool results were pruned (no summary, nothing reindexed).
    pub cut: Option<usize>,
    /// Messages removed (replaced by one summary), or pruned tool results.
    pub removed: usize,
}

/// Reserve-based trigger: compact when the estimate exceeds the window
/// minus the reply reserve.
fn over_pressure(history: &[ChatMessage], context_limit: u64) -> bool {
    estimate_tokens(history) > context_limit.saturating_sub(reserve_tokens(context_limit))
}

/// Summarize `history[1..cut]` and splice in one summary message. Returns
/// `None` when under the trigger (unless `force`); errors when compaction
/// cannot help.
pub async fn compact_history(
    client: &LlmClient,
    model: Option<&str>,
    history: &mut Vec<ChatMessage>,
    context_limit: u64,
    force: bool,
    should_stop: &(dyn Fn() -> bool + Send + Sync),
    on_event: &mut (dyn FnMut(AgentEvent) + Send),
) -> Result<Option<CompactionInfo>> {
    if context_limit == 0 || (!force && !over_pressure(history, context_limit)) {
        return Ok(None);
    }
    // Cheap relief first: trim over-budget tool results in place. When
    // that alone clears the pressure, no summary call is needed at all.
    let pruned = prune_tool_results(history);
    if pruned > 0 && (force || !over_pressure(history, context_limit)) {
        on_event(AgentEvent::Notice {
            text: format!("Trimmed {pruned} oversized tool result(s) to save context."),
        });
        if !force {
            return Ok(Some(CompactionInfo { cut: None, removed: pruned }));
        }
    }
    let Some(cut) = plan_cut(history, KEEP_RECENT_TOKENS) else {
        bail!("Conversation is too long to continue and has no compactable turns — start a new chat (/new)");
    };
    // The kept tail must itself fit with room for the summary and the reply.
    let tail_estimate = estimate_tokens(&history[cut..]);
    if tail_estimate > (context_limit as f64 * 0.9) as u64 {
        bail!("Conversation is too long even after compaction — start a new chat (/new)");
    }
    let mut rendered: Vec<String> = history[1..cut].iter().filter_map(render_for_summary).collect();
    let files = file_ops_section(&history[1..cut]);
    let mut transcript = rendered.join("\n\n");
    if transcript.chars().count() > SUMMARIZE_CAP_CHARS {
        transcript = transcript.chars().take(SUMMARIZE_CAP_CHARS).collect();
    }
    if let Some(files) = files {
        transcript.push_str("\n\n");
        transcript.push_str(&files);
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
    Ok(Some(CompactionInfo { cut: Some(cut), removed }))
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
    fn plan_cut_keeps_budget_on_user_boundary() {
        // Heavy messages (~500 tok each): the walk-back stops near the budget.
        let mut h = vec![ChatMessage::system("sys")];
        for i in 0..20 {
            h.push(user(&format!("question {i}")));
            h.push(ChatMessage::assistant("x".repeat(2000)));
        }
        let cut = plan_cut(&h, 4_096).unwrap();
        assert_eq!(h[cut].role, "user");
        let tail = estimate_tokens(&h[cut..]);
        assert!(tail >= 4_096, "budget reached: {tail}");
        assert!(tail <= 4_096 + 600, "not overshot by more than a message: {tail}");
    }

    #[test]
    fn plan_cut_nothing_to_remove() {
        let h = history(1);
        assert_eq!(plan_cut(&h, 4_096), None);
    }

    #[test]
    fn plan_cut_light_messages_cut_at_first_user() {
        // Light messages never reach the budget — the walk-back spans the
        // whole history and lands on the first user message.
        let h = history(20);
        let cut = plan_cut(&h, 4_096).unwrap();
        assert_eq!(cut, 1);
        assert_eq!(h[cut].role, "user");
    }

    #[test]
    fn plan_cut_skips_existing_summaries() {
        let mut h = history(20);
        // A summary sitting exactly at the walk-back cut must not become it.
        h[1] = ChatMessage::user(format!("{SUMMARY_MARKER} — old]\n\nstuff"));
        let cut = plan_cut(&h, 4_096).unwrap();
        assert_ne!(cut, 1);
        assert_eq!(h[cut].role, "user");
        assert!(!is_summary(&h[cut]));
    }

    #[test]
    fn prune_tool_results_trims_over_budget() {
        let big = "y".repeat(TOOL_RESULT_TRIM_THRESHOLD * 2);
        let mut h = vec![
            ChatMessage::user("q"),
            ChatMessage {
                role: "tool".into(),
                content: Some(Value::String(big.clone())),
                tool_calls: None,
                tool_call_id: Some("c".into()),
            },
            ChatMessage::user("keep"),
        ];
        assert_eq!(prune_tool_results(&mut h), 1);
        let content = h[1].content.as_ref().and_then(|c| c.as_str()).unwrap();
        assert!(content.contains("middle pruned"));
        assert!(content.chars().count() < TOOL_RESULT_TRIM_THRESHOLD);
        // Small results untouched; non-tool messages untouched.
        assert_eq!(prune_tool_results(&mut h), 0);
        h[0].content = Some(Value::String(big.clone()));
        assert_eq!(prune_tool_results(&mut h), 0);
    }

    #[test]
    fn file_ops_section_lists_write_and_edit_paths() {
        use crate::client::{FunctionCall, ToolCall};
        let mk = |name: &str, args: &str| ChatMessage {
            role: "assistant".into(),
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "c".into(),
                call_type: "function".into(),
                function: FunctionCall { name: name.into(), arguments: args.into() },
            }]),
            tool_call_id: None,
        };
        let h = vec![
            mk("write_file", r#"{"path":"src/a.rs"}"#),
            ChatMessage::user("x"),
            mk("edit_file", r#"{"path":"src/a.rs"}"#),
            mk("read_file", r#"{"path":"src/b.rs"}"#),
            mk("edit_file", "not json"),
        ];
        let section = file_ops_section(&h).unwrap();
        assert!(section.contains("src/a.rs"));
        assert!(!section.contains("src/b.rs"), "reads are not file ops");
        assert_eq!(section.matches("src/a.rs").count(), 1, "deduped");
        assert!(file_ops_section(&[ChatMessage::user("q")]).is_none());
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
