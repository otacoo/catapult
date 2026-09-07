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
use crate::permissions::{ApprovalKey, Decision, Grant, PermissionEngine, Scope};
use crate::tools::ToolRegistry;

use serde_json::Value;

pub const DEFAULT_MAX_TURNS: usize = 40;
pub const DEFAULT_SUBAGENT_MAX_TURNS: usize = 25;

// ── Subagent kinds ──────────────────────────────────────────────────────────

/// The two built-in ephemeral specialists. Prompts are deliberately brief —
/// the orchestrator owns planning; specialists execute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentKind {
    Coder,
    Researcher,
}

impl SubagentKind {
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_lowercase().as_str() {
            "coder" => Ok(Self::Coder),
            "researcher" => Ok(Self::Researcher),
            other => anyhow::bail!("Unknown agent type '{other}' (expected 'coder' or 'researcher')"),
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Coder => "coder",
            Self::Researcher => "researcher",
        }
    }

    fn prompt(&self) -> &'static str {
        match self {
            Self::Coder => {
                "You are a focused implementation subagent. You execute exactly one coding task inside a sandboxed project directory, then report back. \
You cannot spawn further subagents. \
Read relevant files before editing; make small, exact edits with edit_file (the search text must match exactly once); create files with write_file; \
verify with search_content or find_files. Allowlisted read-only commands run automatically, anything else needs user approval — if denied, adapt instead of retrying. \
Do not expand the task scope. If the goal is ambiguous, make the most reasonable assumption and note it. \
Your final message is the only thing the orchestrator sees: report what changed (files and a one-line summary each), what you verified, and anything left undone."
            }
            Self::Researcher => {
                "You are an investigation subagent. You answer exactly one question about a sandboxed project directory, then report back. \
You cannot create or modify anything. \
Use find_files and search_content with specific patterns; read only what is needed; verify claims by reading the actual code. \
Your final message is the only thing the orchestrator sees: state the answer directly, with concrete file:line references as evidence, then stop."
            }
        }
    }

    fn allowed_tools(&self) -> &'static [&'static str] {
        match self {
            Self::Coder => &[
                "read_file",
                "write_file",
                "edit_file",
                "find_files",
                "search_content",
                "exec",
            ],
            Self::Researcher => &["read_file", "find_files", "search_content", "exec"],
        }
    }
}

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
    /// A subagent started working (UI shows an inline activity card).
    SubagentSpawned { call_id: String, kind: String, goal: String },
    /// A subagent finished; `summary` is what the orchestrator received.
    SubagentFinished { call_id: String, kind: String, summary: String },
    /// Non-fatal notice for the user (e.g. VRAM feasibility warning).
    Notice { text: String },
}

/// Configuration for ephemeral subagents (enabled = orchestrator may delegate).
pub struct Subagents {
    pub jail: Arc<crate::sandbox::PathJail>,
    pub max_turns: usize,
    /// Model override for subagent workers (hybrid routing); `None` inherits
    /// the orchestrator's model.
    pub model: Option<String>,
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
    /// Reasoning effort hint for reasoning-capable models
    /// ("low"|"medium"|"high"|"max"|"xhigh"; None = server default).
    pub reasoning_effort: Option<String>,
    pub max_turns: usize,
    /// When set, the orchestrator may delegate via `spawn_subagent`. The
    /// subagent runs with a registry stripped of `spawn_subagent` and filtered
    /// to the kind's allowlist — recursion is impossible by construction.
    pub subagents: Option<Subagents>,
}

/// Result of a full agent run: the final answer plus stream metrics for the
/// UI (model name/tokens-per-second footer).
#[derive(Debug, Clone, Default)]
pub struct AgentOutcome {
    pub text: String,
    pub gen_tokens: usize,
    pub prompt_tokens: Option<u64>,
    pub tokens_per_sec: Option<f64>,
    pub elapsed_ms: u64,
}

impl AgentRun<'_> {
    /// Drive the loop to completion. Mutates `history` in place (system +
    /// conversation, including tool messages). Returns the final assistant
    /// text when the model finishes without tool calls.
    pub async fn run(
        &self,
        history: &mut Vec<ChatMessage>,
        should_stop: Arc<dyn Fn() -> bool + Send + Sync>,
        gate: Arc<dyn ApprovalGate>,
        mut on_stream: impl FnMut(StreamEvent) + Send,
        mut on_event: impl FnMut(AgentEvent) + Send,
    ) -> Result<AgentOutcome> {
        let mut turns_used = 0usize;
        let mut sub_seq = 0usize;
        loop {
            if should_stop() {
                bail!("aborted");
            }
            turns_used += 1;
            if turns_used > self.max_turns {
                bail!("Turn budget exhausted ({} turns)", self.max_turns);
            }

            // 1. Stream one completion, accumulating content + tool deltas.
            // Metrics: content/tool deltas ≈ tokens; first-delta → finish
            // gives the tokens-per-second and response time of the answer.
            // Reasoning deltas are forwarded (UI shows "Thinking…") but are
            // excluded from answer text and speed metrics.
            let mut collector = StreamCollector::default();
            let mut text_acc = String::new();
            let mut deltas = 0usize;
            let mut first_delta: Option<std::time::Instant> = None;
            let mut usage_tokens: Option<u64> = None;
            let mut usage_prompt: Option<u64> = None;
            let mut on_delta = |ev: StreamEvent| {
                match &ev {
                    StreamEvent::Content { text } => {
                        text_acc.push_str(text);
                        deltas += 1;
                    }
                    StreamEvent::ToolCallDelta { .. } => deltas += 1,
                    StreamEvent::Usage { prompt_tokens, completion_tokens } => {
                        usage_prompt = Some(*prompt_tokens);
                        usage_tokens = Some(*completion_tokens);
                    }
                    _ => {}
                }
                if first_delta.is_none() {
                    first_delta = Some(std::time::Instant::now());
                }
                collector.push(&ev);
                on_stream(ev);
            };
            let turn_started = std::time::Instant::now();
            let finish = self
                .client
                .chat_stream(
                    self.model.as_deref(),
                    history,
                    Some(&self.registry.tool_schemas()),
                    self.reasoning_effort.as_deref(),
                    &*should_stop,
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
                    content: Some(Value::String(text_acc.clone())),
                    tool_calls: None,
                    tool_call_id: None,
                });
                let elapsed = first_delta
                    .map(|t| t.elapsed())
                    .unwrap_or_else(|| turn_started.elapsed());
                let tokens = usage_tokens.unwrap_or(deltas as u64) as usize;
                let tokens_per_sec = if deltas > 0 {
                    Some(tokens as f64 / elapsed.as_secs_f64().max(0.001))
                } else {
                    None
                };
                let _ = finish;
                return Ok(AgentOutcome {
                    text: text_acc,
                    gen_tokens: tokens,
                    prompt_tokens: usage_prompt,
                    tokens_per_sec: tokens_per_sec.filter(|v| *v > 0.0 && v.is_finite()),
                    elapsed_ms: elapsed.as_millis() as u64,
                });
            }

            // 2. Assistant message with tool calls must precede tool results.
            history.push(ChatMessage {
                role: "assistant".into(),
                content: if text_acc.is_empty() { None } else { Some(Value::String(text_acc)) },
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

                // spawn_subagent is intercepted: the subagent loop runs inline
                // and only its report enters this transcript.
                let output = if tool_name == "spawn_subagent" {
                    match &self.subagents {
                        None => "error: subagents are not available".to_string(),
                        Some(sub) => {
                            let seq = sub_seq;
                            sub_seq += 1;
                            match self
                                .run_subagent(sub, seq, &args_value, gate.clone(), should_stop.clone(), &mut on_event)
                                .await
                            {
                                Ok(report) => report,
                                Err(e) => format!("error: {e:#}"),
                            }
                        }
                    }
                } else {
                    self.execute_tool_call(&tool_name, &args_value, args_pretty, &call_id, &gate, &mut on_event)
                        .await
                };

                history.push(ChatMessage {
                    role: "tool".into(),
                    content: Some(Value::String(output)),
                    tool_calls: None,
                    tool_call_id: Some(call_id),
                });
            }
        }
    }

    /// Permission-gated execution of one non-spawn tool call. Returns the
    /// text that goes back to the model as the tool result.
    async fn execute_tool_call(
        &self,
        tool_name: &str,
        args_value: &Value,
        args_pretty: String,
        call_id: &str,
        gate: &Arc<dyn ApprovalGate>,
        on_event: &mut (dyn FnMut(AgentEvent) + Send),
    ) -> String {
        let Some(tool) = self.registry.get(tool_name) else {
            return format!("error: unknown tool '{tool_name}'");
        };
        // Permission check (read-only tools auto-allow).
        let allowed = match tool.approval_key(args_value) {
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
                            tool: tool_name.to_string(),
                            command: None,
                            scope: match scope {
                                Approved::Once => Scope::Once,
                                Approved::Session => Scope::Session,
                                Approved::Denied => unreachable!(),
                            },
                            expires: None,
                        });
                        // Once-grants are consumed by check.
                        self.engine.check(&ApprovalKey {
                            tool: tool_name.to_string(),
                            command: None,
                        }) == Decision::Allowed
                    }
                },
            },
        };
        if !allowed {
            on_event(AgentEvent::ToolResult {
                call_id: call_id.to_string(),
                ok: false,
                output: "denied by user".into(),
            });
            return "denied by user".to_string();
        }
        let res = self
            .registry
            .spawn_execute(tool_name.to_string(), args_value.clone())
            .await
            .unwrap_or_else(|e| Err(anyhow::Error::new(e)));
        match res {
            Ok(out) => {
                on_event(AgentEvent::ToolResult {
                    call_id: call_id.to_string(),
                    ok: true,
                    output: short_args(&out),
                });
                out
            }
            Err(e) => {
                let msg = format!("error: {e:#}");
                on_event(AgentEvent::ToolResult {
                    call_id: call_id.to_string(),
                    ok: false,
                    output: short_args(&msg),
                });
                msg
            }
        }
    }

    /// Run an ephemeral subagent to completion and return its final report
    /// (truncated for the orchestrator transcript). Nested tool events are
    /// forwarded with a `sub:` call-id prefix so the UI can group them.
    fn run_subagent<'a>(
        &'a self,
        sub: &'a Subagents,
        seq: usize,
        args: &'a Value,
        gate: Arc<dyn ApprovalGate>,
        should_stop: Arc<dyn Fn() -> bool + Send + Sync>,
        on_event: &'a mut (dyn FnMut(AgentEvent) + Send),
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
        let goal = args
            .get("goal")
            .and_then(|g| g.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let kind = SubagentKind::parse(args.get("agent_type").and_then(|a| a.as_str()).unwrap_or(""));
        let gate = gate.clone();
        Box::pin(async move {
            let kind = kind?;
            if goal.is_empty() {
                bail!("spawn_subagent requires a non-empty 'goal'");
            }
            let call_ref = format!("subagent-{seq}");
            on_event(AgentEvent::SubagentSpawned {
                call_id: call_ref.clone(),
                kind: kind.name().into(),
                goal: goal.clone(),
            });

            // Fresh, isolated transcript: system prompt + goal (+ ctx files).
            let mut history = vec![ChatMessage::system(kind.prompt().to_string())];
            let mut initial = format!("Goal: {goal}\n");
            if let Some(files) = args.get("ctx_files").and_then(|f| f.as_array()) {
                for f in files {
                    if let Some(path) = f.as_str() {
                        let cap = 20_000usize;
                        match sub.jail.check_read(std::path::Path::new(path)) {
                            Ok(resolved) => match std::fs::read_to_string(&resolved) {
                                Ok(content) => {
                                    let mut shown: String =
                                        content.chars().take(cap).collect();
                                    if content.chars().count() > cap {
                                        shown.push_str("\n[truncated]");
                                    }
                                    initial.push_str(&format!("\nContext file {path}:\n```\n{shown}\n```\n"));
                                }
                                Err(_) => initial.push_str(&format!("\nContext file {path}: could not be read.\n")),
                            },
                            Err(_) => initial.push_str(&format!("\nContext file {path}: outside the sandbox.\n")),
                        }
                    }
                }
            }
            history.push(ChatMessage::user(initial));

            let registry = Arc::new(
                ToolRegistry::project_tools(sub.jail.clone())
                    .without(&["spawn_subagent"])
                    .only(kind.allowed_tools()),
            );
            let run = AgentRun {
                client: self.client,
                registry,
                engine: self.engine.clone(),
                model: sub.model.clone().or_else(|| self.model.clone()),
                reasoning_effort: self.reasoning_effort.clone(),
                max_turns: sub.max_turns,
                subagents: None, // no recursion: the strip above is belt-and-braces
            };
            let mut nested = |ev: AgentEvent| {
                let ev = match ev {
                    AgentEvent::ToolCall { call_id, tool, args } => AgentEvent::ToolCall {
                        call_id: format!("sub:{call_id}"),
                        tool,
                        args,
                    },
                    AgentEvent::ToolResult { call_id, ok, output } => AgentEvent::ToolResult {
                        call_id: format!("sub:{call_id}"),
                        ok,
                        output,
                    },
                    other => other,
                };
                on_event(ev);
            };
            let mut noop = |_ev: StreamEvent| {};
            let outcome = run
                .run(&mut history, should_stop.clone(), gate, &mut noop, &mut nested)
                .await?;

            // Cap the report entering the orchestrator transcript.
            const REPORT_CAP: usize = 16_000;
            let report = if outcome.text.chars().count() > REPORT_CAP {
                let mut t: String = outcome.text.chars().take(REPORT_CAP).collect();
                t.push_str("\n[report truncated]");
                t
            } else {
                outcome.text
            };
            on_event(AgentEvent::SubagentFinished {
                call_id: call_ref,
                kind: kind.name().into(),
                summary: short_args(&report),
            });
            Ok(report)
        })
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
        let ev = AgentEvent::SubagentSpawned {
            call_id: "subagent-0".into(),
            kind: "coder".into(),
            goal: "add tests".into(),
        };
        assert_eq!(serde_json::to_value(&ev).unwrap()["type"], "subagent_spawned");
    }

    #[test]
    fn subagent_kind_parsing() {
        assert!(matches!(SubagentKind::parse("coder"), Ok(SubagentKind::Coder)));
        assert!(matches!(SubagentKind::parse("Researcher"), Ok(SubagentKind::Researcher)));
        assert!(SubagentKind::parse("").is_err());
        assert!(SubagentKind::parse("orchestrator").is_err());
        assert!(!SubagentKind::Coder.prompt().is_empty());
        assert!(!SubagentKind::Researcher.prompt().is_empty());
    }

    #[test]
    fn subagent_registries_strip_recursion() {
        let dir = std::env::temp_dir().join(format!("harness-sub-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let jail = Arc::new(crate::sandbox::PathJail::new(&dir, &[], &[]).unwrap());
        let coder = ToolRegistry::project_tools(jail.clone())
            .without(&["spawn_subagent"])
            .only(SubagentKind::Coder.allowed_tools());
        assert!(coder.get("spawn_subagent").is_none(), "recursion must be impossible");
        assert!(coder.get("write_file").is_some());
        assert!(coder.get("exec").is_some());

        let researcher = ToolRegistry::project_tools(jail).without(&["spawn_subagent"]).only(SubagentKind::Researcher.allowed_tools());
        assert!(researcher.get("read_file").is_some());
        assert!(researcher.get("write_file").is_none(), "researcher must be read-only");
        assert!(researcher.get("edit_file").is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
