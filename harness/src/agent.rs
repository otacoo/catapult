//! Orchestrator loop: LLM ⇄ sandboxed tools.
//!
//! One turn = stream a chat completion (with the tool registry attached),
//! collect tool calls, execute them under the path jail + permission engine,
//! append tool results, repeat until the model answers without tool calls or
//! the turn budget is exhausted.
//!
//! Approval flow: when a call needs a grant, the loop parks on an
//! [`ApprovalGate`] (the UI implementation emits a Tauri event and awaits the
//! user's decision). An approved call is granted (`Once`/`Session`) and then
//! executed through the normal permission check; a denial feeds "denied by
//! user" back to the model as the tool result, never silently retrying.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::client::{ChatMessage, LlmClient, StreamCollector, StreamEvent};
use crate::permissions::{Decision, Grant, PermissionEngine, Scope};
use crate::tools::ToolRegistry;

pub const DEFAULT_MAX_TURNS: usize = 40;

/// Args summary shown in tool-call cards (kept short; full args live in the
/// call itself).
fn short_args(pretty: &str) -> String {
    let one_line = pretty.lines().collect::<Vec<_>>().join(" ");
    let mut s = one_line.chars().take(300).collect::<String>();
    if s.len() < one_line.len() {
        s.push('…');
    }
    s
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    /// A tool call was issued (UI card with name + args).
    ToolCall { call_id: String, tool: String, args: String },
    /// Tool finished (output truncated for the orchestrator transcript).
    ToolResult { call_id: String, ok: bool, output: String },
    /// A grant is required; the UI must show the approval prompt.
    ApprovalRequired { tool: String, command: Option<String>, args: String },
}

/// What the UI answers when asked about a suspicious call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Approved {
    Denied,
    Once,
    Session,
}

/// Request handed to the gate when a call needs a grant.
pub struct ApprovalRequest {
    pub key: crate::permissions::ApprovalKey,
    /// Pretty-printed arguments for the approval card.
    pub args_pretty: String,
}

/// Bridge the loop waits on for user decisions. Implemented in src-tauri
/// (emit Tauri event → await oneshot from the approve command).
pub trait ApprovalGate: Send + Sync {
    fn decide(&self, req: ApprovalRequest) -> Pin<Box<dyn Future<Output = Approved> + Send>>;
}

pub struct AgentRun<'a> {
    pub client: &'a LlmClient,
    pub registry: Arc<ToolRegistry>,
    pub engine: Arc<PermissionEngine>,
    pub model: Option<String>,
    pub max_turns: usize,
}

impl AgentRun<'_> {
    /// Drive the loop to completion. Mutates `history` in place (system +
    /// conversation, including tool messages). Returns the final assistant
    /// text when the model finishes without tool calls.
    pub async fn run(
        &self,
        history: &mut Vec<ChatMessage>,
        should_stop: impl Fn() -> bool,
        gate: Arc<dyn ApprovalGate>,
        mut on_stream: impl FnMut(StreamEvent) + Send,
        mut on_event: impl FnMut(AgentEvent) + Send,
    ) -> Result<String> {
        let _tools = self.registry.tool_schemas();
        let mut turns_used = 0usize;
        loop {
            if should_stop() {
                bail!("aborted");
            }
            turns_used += 1;
            if turns_used > self.max_turns {
                bail!("Turn budget exhausted ({} turns)", self.max_turns);
            }

            // 1. Stream one completion, accumulating content + tool deltas.
            let collector = StreamCollector::default();
            let mut text_acc = String::new();
            let mut on_delta = |ev: StreamEvent| {
                if let StreamEvent::Content { text } = &ev {
                    text_acc.push_str(text);
                }
                on_stream(ev);
            };
            let finish = self
                .client
                .chat_stream(
                    self.model.as_deref(),
                    history,
                    Some(&self.registry.tool_schemas()),
                    &should_stop,
                    &mut on_delta,
                )
                .await?;
            if should_stop() {
                bail!("aborted");
            }

            let calls = collector.finish()?;
            if calls.is_empty() {
                history.push(ChatMessage {
                    role: "assistant".into(),
                    content: Some(text_acc.clone()),
                    tool_calls: None,
                    tool_call_id: None,
                });
                let _ = finish;
                return Ok(text_acc);
            }

            // 2. Assistant message with tool calls must precede tool results.
            history.push(ChatMessage {
                role: "assistant".into(),
                content: if text_acc.is_empty() { None } else { Some(text_acc) },
                tool_calls: Some(calls.clone()),
                tool_call_id: None,
            });

            // 3. Execute calls one by one under jail + permissions.
            for call in calls {
                if should_stop() {
                    bail!("aborted");
                }
                let call_id = call.id.clone();
                let tool_name = call.function.name.clone();
                let args_value: serde_json::Value =
                    serde_json::from_str(&call.function.arguments).unwrap_or_else(|_| json!({}));
                let args_pretty =
                    serde_json::to_string_pretty(&args_value).unwrap_or_else(|_| call.function.arguments.clone());
                on_event(AgentEvent::ToolCall {
                    call_id: call_id.clone(),
                    tool: tool_name.clone(),
                    args: short_args(&args_pretty),
                });

                let output = match self.registry.get(&tool_name) {
                    None => format!("error: unknown tool '{tool_name}'"),
                    Some(tool) => {
                        // Permission check (read-only tools auto-allow).
                        let allowed = match tool.approval_key(&args_value) {
                            None => true,
                            Some(key) => match self.engine.check(&key) {
                                Decision::Allowed => true,
                                Decision::NeedsApproval => match gate
                                    .decide(ApprovalRequest {
                                        args_pretty: args_pretty.clone(),
                                        key,
                                    })
                                    .await
                                {
                                    Approved::Denied => false,
                                    scope => {
                                        self.engine.grant(Grant {
                                            tool: tool_name.clone(),
                                            command: None,
                                            scope: match scope {
                                                Approved::Once => Scope::Once,
                                                Approved::Session => Scope::Session,
                                                Approved::Denied => unreachable!(),
                                            },
                                            expires: None,
                                        });
                                        // Once-grants are consumed by check.
                                        self.engine.check(&crate::permissions::ApprovalKey {
                                            tool: tool_name.clone(),
                                            command: None,
                                        }) == Decision::Allowed
                                    }
                                },
                            },
                        };
                        if !allowed {
                            "denied by user".to_string()
                        } else {
                            let res = self
                                .registry
                                .spawn_execute(tool_name.clone(), args_value.clone())
                                .await
                                .unwrap_or_else(|e| Err(anyhow::Error::new(e)));
                            match res {
                                Ok(out) => {
                                    let truncated = short_args(&out);
                                    on_event(AgentEvent::ToolResult {
                                        call_id: call_id.clone(),
                                        ok: true,
                                        output: truncated,
                                    });
                                    out
                                }
                                Err(e) => {
                                    let msg = format!("error: {e:#}");
                                    on_event(AgentEvent::ToolResult {
                                        call_id: call_id.clone(),
                                        ok: false,
                                        output: short_args(&msg),
                                    });
                                    msg
                                }
                            }
                        }
                    }
                };

                history.push(ChatMessage {
                    role: "tool".into(),
                    content: Some(output),
                    tool_calls: None,
                    tool_call_id: Some(call_id),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::FunctionCall;

    // Note: the loop itself requires a live SSE endpoint to unit-test; the
    // pieces it composes (StreamCollector, ToolRegistry, PermissionEngine,
    // PathJail) each have their own suites. Message assembly is covered here.
    #[test]
    fn short_args_truncates() {
        let long = "x".repeat(500);
        let s = short_args(&long);
        assert!(s.ends_with('…'));
        assert!(s.chars().count() <= 301);
    }

    #[test]
    fn assistant_tool_message_shape_is_openai_compatible() {
        let call = crate::client::ToolCall {
            id: "call_1".into(),
            call_type: "function".into(),
            function: FunctionCall { name: "read_file".into(), arguments: "{}".into() },
        };
        let msg = ChatMessage {
            role: "assistant".into(),
            content: None,
            tool_calls: Some(vec![call]),
            tool_call_id: None,
        };
        let v = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["role"], "assistant");
        assert_eq!(v["tool_calls"][0]["function"]["name"], "read_file");
        assert!(v.get("content").is_none(), "empty content must be omitted");
        let tool_msg = ChatMessage {
            role: "tool".into(),
            content: Some("data".into()),
            tool_calls: None,
            tool_call_id: Some("call_1".into()),
        };
        let v = serde_json::to_value(&tool_msg).unwrap();
        assert_eq!(v["tool_call_id"], "call_1");
        assert_eq!(v["role"], "tool");
    }

    #[test]
    fn agent_event_serializes_with_type_tags() {
        let ev = AgentEvent::ToolCall {
            call_id: "c1".into(),
            tool: "write_file".into(),
            args: "{}".into(),
        };
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["type"], "tool_call");
        let ev = AgentEvent::ApprovalRequired {
            tool: "exec".into(),
            command: Some("npm".into()),
            args: "{}".into(),
        };
        assert_eq!(serde_json::to_value(&ev).unwrap()["type"], "approval_required");
    }
}
