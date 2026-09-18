//! Orchestrator loop: LLM ⇄ sandboxed tools.
//!
//! Denials feed "denied by user" back to the model; never silently retry.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::client::{ChatMessage, LlmClient, StreamCollector, StreamEvent};
use crate::permissions::{Decision, Grant, PermissionEngine, Scope};
use crate::tools::ToolRegistry;
use crate::tools::Tool as _;

use serde_json::Value;

pub const DEFAULT_MAX_TURNS: usize = 40;
pub const DEFAULT_SUBAGENT_MAX_TURNS: usize = 25;

// ── Subagent kinds ──────────────────────────────────────────────────────────

/// Ephemeral specialists; prompts stay brief — the orchestrator owns planning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentKind {
    Coder,
    Researcher,
}

/// Shared shell guidance so workers use the right idioms. Byte-stable prompt prefix.
pub fn os_shell_snippet() -> String {
    let (os_name, shell, shell_examples, avoid) = if cfg!(windows) {
        (
            "Windows",
            "PowerShell (`powershell -NoProfile -Command ...`)",
            "Get-ChildItem, Get-Content, Select-String; separate statements with `;`",
            "sh/bash syntax (`ls -la`, `&&`, `grep`, `/dev/null`, leading `/` paths)",
        )
    } else if cfg!(target_os = "macos") {
        (
            "macOS",
            "POSIX sh (`sh -c ...`)",
            "ls, cat, grep; separate statements with `&&` or `;`",
            "PowerShell syntax (`Get-ChildItem`, `;` only quirks aside)",
        )
    } else {
        (
            "Linux",
            "POSIX sh (`sh -c ...`)",
            "ls, cat, grep; separate statements with `&&` or `;`",
            "PowerShell syntax (`Get-ChildItem`, `;` only quirks aside)",
        )
    };
    format!(
        "You run on {os_name}. Shell commands execute via {shell}: use {os_name} syntax ({shell_examples}) — never {avoid}."
    )
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

    fn prompt(&self, skills: &[crate::skills::Skill]) -> String {
        let base: &'static str = match self {
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
        };
        format!("{base} {}{}", os_shell_snippet(), crate::skills::system_prompt_listing(skills))
    }

    /// Subagent registry: allowlist + skills, MCP for coder only. Strips `spawn_subagent`.
    pub fn subagent_registry(
        jail: Arc<crate::sandbox::PathJail>,
        skills: &[crate::skills::Skill],
        mcp_tools: &[crate::mcp::McpTool],
        kind: SubagentKind,
        exec_enabled: bool,
    ) -> Arc<ToolRegistry> {
        let mut registry =
            ToolRegistry::project_tools(jail).without(&["spawn_subagent"]);
        if !exec_enabled {
            registry = registry.without(&["exec"]);
        }
        let mut allowed: Vec<String> =
            kind.allowed_tools().iter().map(|s| s.to_string()).collect();
        if !skills.is_empty() {
            registry = registry.add(Arc::new(crate::skills::SkillTool::new(skills.to_vec())));
            allowed.push("skill".to_string());
        }
        if kind == SubagentKind::Coder {
            for tool in mcp_tools {
                allowed.push(tool.name());
                registry = registry.add(Arc::new(tool.clone()));
            }
        }
        let refs: Vec<&str> = allowed.iter().map(|s| s.as_str()).collect();
        Arc::new(registry.only(&refs))
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

fn short_args(pretty: &str) -> String {
    let one_line = pretty.lines().collect::<Vec<_>>().join(" ");
    let mut s = one_line.chars().take(300).collect::<String>();
    if s.len() < one_line.len() {
        s.push('…');
    }
    s
}

/// llama-server's context-overflow rejection, matched loosely across builds
/// ("the request exceeds the available context size", "too many tokens", …).
fn is_context_overflow(err: &str) -> bool {
    let l = err.to_lowercase();
    (l.contains("exceed") && (l.contains("context") || l.contains("token")))
        || (l.contains("context") && l.contains("overflow"))
        || l.contains("too many tokens")
        || l.contains("maximum context length")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    ToolCall { call_id: String, tool: String, args: String },
    /// `images` carries live mid-run reads for the UI.
    ToolResult { call_id: String, ok: bool, output: String, images: Vec<String> },
    ApprovalRequired { tool: String, command: Option<String>, args: String },
    /// `branch` is set for sibling-worktree runs.
    SubagentSpawned { call_id: String, kind: String, goal: String, branch: Option<String> },
    SubagentFinished { call_id: String, kind: String, summary: String },
    /// Transcript was compacted mid-run; the UI shows how much was folded away.
    Compacted { removed: usize },
    Notice { text: String },
}

pub struct Subagents {
    pub jail: Arc<crate::sandbox::PathJail>,
    pub max_turns: usize,
    /// `None` inherits the orchestrator's model.
    pub model: Option<String>,
    pub skills: Vec<crate::skills::Skill>,
    /// Coder kind only; researcher stays read-only (MCP can mutate the world).
    pub mcp_tools: Vec<crate::mcp::McpTool>,
    pub vision: bool,
    /// Follows the Tools page shell toggle; off removes `exec` everywhere.
    pub exec_enabled: bool,
    /// `image_url` parts forwarded so visual work can be delegated.
    pub images: Vec<serde_json::Value>,
    pub attachment_texts: Vec<String>,
    /// The WORKER model's own context window (role override / launch ctx);
    /// None = inherit the orchestrator's limit.
    pub context_limit: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Approved {
    Denied,
    Once,
    Session,
    Project,
    Global,
}

pub struct ApprovalRequest {
    pub key: crate::permissions::ApprovalKey,
    pub args_pretty: String,
}

/// Implemented in src-tauri (Tauri event → oneshot from the approve command).
pub trait ApprovalGate: Send + Sync {
    fn decide(&self, req: ApprovalRequest) -> Pin<Box<dyn Future<Output = Approved> + Send>>;
    /// Save hook after a persistable grant lands; no-op by default.
    fn grants_changed(&self, _grants: &[crate::permissions::Grant]) {}
}

pub struct AgentRun<'a> {
    pub client: &'a LlmClient,
    pub registry: Arc<ToolRegistry>,
    pub engine: Arc<PermissionEngine>,
    pub model: Option<String>,
    /// Subagent runs inherit the parent's project.
    pub project: Option<String>,
    /// Reasoning effort hint ("low"|"medium"|"high"|"max"|"xhigh"; None = default).
    pub reasoning_effort: Option<String>,
    pub max_turns: usize,
    /// When set, delegation allowed; registry strips `spawn_subagent` so recursion is impossible.
    pub subagents: Option<Subagents>,
    /// Only vision runs receive image parts (pixels 500 text-only models).
    pub vision: bool,
    /// Effective context size for compaction; None disables it.
    pub context_limit: Option<u64>,
    /// Compaction cuts (first-kept index) recorded as they happen — read by
    /// the caller after ANY outcome so meta reindexing survives errors too.
    pub compactions_log: Arc<Mutex<Vec<usize>>>,
}

#[derive(Debug, Clone, Default)]
pub struct AgentOutcome {
    pub text: String,
    pub gen_tokens: usize,
    pub prompt_tokens: Option<u64>,
    pub tokens_per_sec: Option<f64>,
    pub elapsed_ms: u64,
    /// Accumulated reasoning; never sent back to the model.
    pub reasoning: String,
    /// Messages removed per compaction, in order (for footer-meta reindexing).
    pub compactions: Vec<usize>,
}

impl AgentRun<'_> {
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
            let mut reasoning_acc = String::new();
            let mut compactions: Vec<usize> = Vec::new();
        loop {
            if should_stop() {
                bail!("aborted");
            }
            // Fold the oldest turns into a summary before they overflow the
            // window; free (no turn spent) and checked every iteration.
            if let Some(limit) = self.context_limit {
                if let Some(info) = crate::compact::compact_history(
                    self.client,
                    self.model.as_deref(),
                    history,
                    limit,
                    false,
                    &*should_stop,
                    &mut on_event,
                )
                .await?
                {
                    compactions.push(info.cut);
                    self.compactions_log.lock().unwrap().push(info.cut);
                }
            }
            turns_used += 1;
            if turns_used > self.max_turns {
                bail!("Turn budget exhausted ({} turns)", self.max_turns);
            }

            // Deltas ≈ tokens; reasoning excluded from metrics.
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
                    StreamEvent::ReasoningDelta { text } => {
                        reasoning_acc.push_str(text);
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
            // Overflow recovery (dsh pattern): a confirmed context-overflow
            // error condenses the history and retries ONCE — a self-heal that
            // beats a hard failure when the estimate lagged reality.
            let mut finish = self
                .client
                .chat_stream(
                    self.model.as_deref(),
                    history,
                    Some(&self.registry.tool_schemas()),
                    self.reasoning_effort.as_deref(),
                    &*should_stop,
                    &mut on_delta,
                )
                .await;
            if let Err(e) = &finish {
                if is_context_overflow(&e.to_string()) && self.context_limit.is_some() {
                    on_event(AgentEvent::Notice {
                        text: "Context overflow — compacting and retrying…".into(),
                    });
                    match crate::compact::compact_history(
                        self.client,
                        self.model.as_deref(),
                        history,
                        self.context_limit.unwrap_or(u64::MAX),
                        true,
                        &*should_stop,
                        &mut on_event,
                    )
                    .await
                    {
                        Ok(Some(info)) => {
                            compactions.push(info.cut);
                            self.compactions_log.lock().unwrap().push(info.cut);
                            finish = self
                                .client
                                .chat_stream(
                                    self.model.as_deref(),
                                    history,
                                    Some(&self.registry.tool_schemas()),
                                    self.reasoning_effort.as_deref(),
                                    &*should_stop,
                                    &mut on_delta,
                                )
                                .await;
                        }
                        // Nothing to cut or summarizer failed — surface the
                        // original overflow error.
                        _ => {}
                    }
                }
            }
            let finish = finish?;
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
                    reasoning: std::mem::take(&mut reasoning_acc),
                    compactions,
                });
            }

            // 2. Assistant message with tool calls must precede tool results.
            history.push(ChatMessage {
                role: "assistant".into(),
                content: if text_acc.is_empty() { None } else { Some(Value::String(text_acc)) },
                tool_calls: Some(calls.clone()),
                tool_call_id: None,
            });

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

                // `spawn_subagent` runs inline; only its report enters the transcript.
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
                    let (output, media) = self.execute_tool_call(&tool_name, &args_value, args_pretty, &call_id, &gate, &mut on_event)
                        .await;
                    history.push(ChatMessage {
                        role: "tool".into(),
                        content: Some(Value::String(output)),
                        tool_calls: None,
                        tool_call_id: Some(call_id),
                    });
                    // Tool results are text-only; pixels ride as a follow-up user message.
                    if !media.is_empty() {
                        let mut parts = vec![serde_json::json!({
                            "type": "text",
                            "text": "Visual context for the tool result above."
                        })];
                        parts.extend(media);
                        history.push(ChatMessage {
                            role: "user".into(),
                            content: Some(Value::Array(parts)),
                            tool_calls: None,
                            tool_call_id: None,
                        });
                    }
                    continue;
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

    async fn execute_tool_call(
        &self,
        tool_name: &str,
        args_value: &Value,
        args_pretty: String,
        call_id: &str,
        gate: &Arc<dyn ApprovalGate>,
        on_event: &mut (dyn FnMut(AgentEvent) + Send),
    ) -> (String, Vec<Value>) {
        let Some(tool) = self.registry.get(tool_name) else {
            return (format!("error: unknown tool '{tool_name}'"), Vec::new());
        };
        let project = self.project.as_deref();
        let allowed = match tool.approval_key(args_value) {
            None => true,
            Some(key) => match self.engine.check(&key, project) {
                Decision::Allowed => true,
                Decision::NeedsApproval => match gate
                    .decide(ApprovalRequest {
                        args_pretty: args_pretty.clone(),
                        key: key.clone(),
                    })
                    .await
                {
                    Approved::Denied => false,
                    scope => {
                        // Grant the approval key (never the bare registry name).
                        self.engine.grant(Grant {
                            tool: key.tool.clone(),
                            command: key.command.clone(),
                            scope: match scope {
                                Approved::Once => Scope::Once,
                                Approved::Session => Scope::Session,
                                Approved::Project => Scope::Project,
                                Approved::Global => Scope::Global,
                                Approved::Denied => unreachable!(),
                            },
                            expires: None,
                            project: self.project.clone(),
                        });
                        gate.grants_changed(&self.engine.persistable());
                        self.engine.check(&key, project) == Decision::Allowed
                    }
                },
            },
        };
        if !allowed {
            on_event(AgentEvent::ToolResult {
                call_id: call_id.to_string(),
                ok: false,
                output: "denied by user".into(),
                images: Vec::new(),
            });
            return ("denied by user".to_string(), Vec::new());
        }
        let res = self
            .registry
            .spawn_execute_with_media(tool_name.to_string(), args_value.clone())
            .await
            .unwrap_or_else(|e| (Err(anyhow::Error::new(e)), Vec::new()));
        match res {
            (Ok(out), media) => {
                // Pixels 500 text-only models; others get a delegation nudge.
                let (text, kept) = if media.is_empty() || self.vision {
                    (out, media)
                } else {
                    (
                        format!("{out}\n[not shown to you: this model has no vision — delegate visual work with spawn_subagent]"),
                        Vec::new(),
                    )
                };
                let images: Vec<String> = kept
                    .iter()
                    .filter_map(|p| {
                        p.get("image_url")?.get("url")?.as_str().map(str::to_string)
                    })
                    .collect();
                on_event(AgentEvent::ToolResult {
                    call_id: call_id.to_string(),
                    ok: true,
                    output: short_args(&text),
                    images,
                });
                (text, kept)
            }
            (Err(e), _) => {
                let msg = format!("error: {e:#}");
                on_event(AgentEvent::ToolResult {
                    call_id: call_id.to_string(),
                    ok: false,
                    output: short_args(&msg),
                    images: Vec::new(),
                });
                (msg, Vec::new())
            }
        }
    }

    async fn ensure_router_model(
        client: &LlmClient,
        id: &str,
        should_stop: &Arc<dyn Fn() -> bool + Send + Sync>,
        on_event: &mut (dyn FnMut(AgentEvent) + Send),
    ) -> Result<()> {
        const POLL_SECS: u64 = 2;
        const TIMEOUT_SECS: u64 = 900;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(TIMEOUT_SECS);
        let mut noticed = false;
        loop {
            if should_stop() {
                bail!("aborted");
            }
            let models = client
                .router_models()
                .await
                .context("router /models failed")?;
            match models.iter().find(|m| m.id == id) {
                Some(m) if m.status == "loaded" => return Ok(()),
                Some(m) if m.status == "failed" => {
                    bail!("Model '{id}' failed to load — check Server Logs for the child error")
                }
                _ => {
                    if !noticed {
                        noticed = true;
                        on_event(AgentEvent::Notice {
                            text: format!(
                                "Loading worker model '{id}' — first subagent run may take a while."
                            ),
                        });
                    }
                    let _ = client.router_load(id).await;
                    if std::time::Instant::now() >= deadline {
                        bail!("Timed out waiting for worker model '{id}' to load");
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(POLL_SECS)).await;
                }
            }
        }
    }

    /// Run a subagent; nested events forwarded with a `sub:` prefix.
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
            // Branch runs isolated in a sibling worktree; never fall back silently.
            let branch = args
                .get("branch")
                .and_then(|b| b.as_str())
                .map(str::trim)
                .filter(|b| !b.is_empty())
                .map(str::to_string);
            let (work_root, worktree_note) = match branch.as_deref() {
                Some(b) => {
                    let (path, created) = crate::git::ensure_worktree(sub.jail.root(), b)
                        .with_context(|| format!("Cannot prepare worktree for branch '{b}'"))?;
                    let note = format!(
                        "Working directory: {} (git worktree on branch '{b}', sibling of the project — {}). Your edits land here, not in the main checkout.",
                        path.display(),
                        if created { "freshly created" } else { "reused" },
                    );
                    (path, Some(note))
                }
                None => (sub.jail.root().to_path_buf(), None),
            };
            // Worktree jail inherits the parent's extra scope.
            let work_jail = Arc::new(sub.jail.rooted_at(&work_root).map_err(|e| {
                anyhow::anyhow!("Cannot sandbox worktree {}: {e:#}", work_root.display())
            })?);
            let call_ref = format!("subagent-{seq}");
            on_event(AgentEvent::SubagentSpawned {
                call_id: call_ref.clone(),
                kind: kind.name().into(),
                goal: goal.clone(),
                branch: branch.clone(),
            });

            // Lazy worker load: eager double-load crawls VRAM-tight machines.
            if let Some(worker) = &sub.model {
                Self::ensure_router_model(self.client, worker, &should_stop, on_event).await?;
            }

            let mut history = vec![ChatMessage::system(kind.prompt(&sub.skills))];
            let mut initial = match &worktree_note {
                Some(note) => format!("{note}\nGoal: {goal}\n"),
                None => format!("Goal: {goal}\n"),
            };
            for block in &sub.attachment_texts {
                initial.push_str(&format!("\n{block}\n"));
            }
            if let Some(files) = args.get("ctx_files").and_then(|f| f.as_array()) {
                for f in files {
                    if let Some(path) = f.as_str() {
                        let cap = 20_000usize;
                        match work_jail.check_read(std::path::Path::new(path)) {
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
            if sub.images.is_empty() {
                history.push(ChatMessage::user(initial));
            } else {
                let mut parts = vec![serde_json::json!({ "type": "text", "text": initial })];
                parts.extend(sub.images.clone());
                history.push(ChatMessage {
                    role: "user".into(),
                    content: Some(serde_json::Value::Array(parts)),
                    tool_calls: None,
                    tool_call_id: None,
                });
            }

            let registry = SubagentKind::subagent_registry(
                work_jail,
                &sub.skills,
                &sub.mcp_tools,
                kind,
                sub.exec_enabled,
            );
            let run = AgentRun {
                client: self.client,
                registry,
                engine: self.engine.clone(),
                model: sub.model.clone().or_else(|| self.model.clone()),
                project: self.project.clone(),
                reasoning_effort: self.reasoning_effort.clone(),
                max_turns: sub.max_turns,
                subagents: None, // stripped above; belt-and-braces
                vision: sub.vision,
                // The worker compacts under ITS model's window (role ctx
                // override), falling back to the orchestrator's.
                context_limit: sub.context_limit.or(self.context_limit),
                // Separate log: the nested run's cuts reindex its own meta scope.
                compactions_log: Arc::new(Mutex::new(Vec::new())),
            };
            let mut nested = |ev: AgentEvent| {
                let ev = match ev {
                    AgentEvent::ToolCall { call_id, tool, args } => AgentEvent::ToolCall {
                        call_id: format!("sub:{call_id}"),
                        tool,
                        args,
                    },
                    AgentEvent::ToolResult { call_id, ok, output, images } => AgentEvent::ToolResult {
                        call_id: format!("sub:{call_id}"),
                        ok,
                        output,
                        images,
                    },
                    // A subagent compacting ITS OWN history is not a main-
                    // transcript event; demote it so the UI never claims the
                    // parent transcript was touched.
                    AgentEvent::Compacted { removed } => AgentEvent::Notice {
                        text: format!("Subagent compacted {removed} of its own messages."),
                    },
                    other => other,
                };
                on_event(ev);
            };
            let mut noop = |_ev: StreamEvent| {};
            let outcome = run
                .run(&mut history, should_stop.clone(), gate, &mut noop, &mut nested)
                .await?;

            // Cap the report; tag worktree edits so the orchestrator knows where they landed.
            const REPORT_CAP: usize = 16_000;
            let mut report = if outcome.text.chars().count() > REPORT_CAP {
                let mut t: String = outcome.text.chars().take(REPORT_CAP).collect();
                t.push_str("\n[report truncated]");
                t
            } else {
                outcome.text
            };
            // Worktree edits are not in the main checkout.
            if let Some(b) = branch.as_deref() {
                report.push_str(&format!(
                    "\n[worktree: {} (branch '{b}') — changes are in the worktree, merge or review them from the sidebar]",
                    work_root.display()
                ));
            }
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

    // Loop needs a live SSE endpoint; composed units have their own suites.
    #[test]
    fn is_context_overflow_matches_build_wordings() {
        assert!(is_context_overflow(
            "Chat request failed (400): the request exceeds the available context size"
        ));
        assert!(is_context_overflow("context overflow in request"));
        assert!(is_context_overflow("error: too many tokens"));
        assert!(is_context_overflow("maximum context length is 4096 tokens"));
        assert!(!is_context_overflow("Chat request failed (500): internal error"));
        assert!(!is_context_overflow("model failed to load"));
    }

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
            branch: Some("feature-x".into()),
        };
        assert_eq!(serde_json::to_value(&ev).unwrap()["type"], "subagent_spawned");
    }

    #[test]
    fn subagent_kind_parsing() {
        assert!(matches!(SubagentKind::parse("coder"), Ok(SubagentKind::Coder)));
        assert!(matches!(SubagentKind::parse("Researcher"), Ok(SubagentKind::Researcher)));
        assert!(SubagentKind::parse("").is_err());
        assert!(SubagentKind::parse("orchestrator").is_err());
        assert!(!SubagentKind::Coder.prompt(&[]).is_empty());
        assert!(!SubagentKind::Researcher.prompt(&[]).is_empty());
    }

    #[test]
    fn subagent_prompts_carry_os_shell_guidance() {
        // Small worker models default to the wrong shell without this.
        let snippet = os_shell_snippet();
        assert!(snippet.contains("Shell commands execute via"));
        if cfg!(windows) {
            assert!(snippet.contains("PowerShell"));
        } else {
            assert!(snippet.contains("POSIX sh"));
        }
        assert!(SubagentKind::Coder.prompt(&[]).contains(&snippet));
        assert!(SubagentKind::Researcher.prompt(&[]).contains(&snippet));
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

    #[test]
    fn subagent_registries_compose_skills() {
        let dir = std::env::temp_dir().join(format!("harness-sub-skills-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let jail = Arc::new(crate::sandbox::PathJail::new(&dir, &[], &[]).unwrap());
        let skills = vec![crate::skills::Skill {
            name: "acme-deploy".to_string(),
            description: "Ship it".to_string(),
            path: dir.join("SKILL.md"),
        }];
        let coder = SubagentKind::subagent_registry(jail.clone(), &skills, &[], SubagentKind::Coder, true);
        assert!(coder.get("skill").is_some(), "coder loads skills on demand");
        assert!(coder.get("spawn_subagent").is_none(), "recursion must be impossible");
        assert!(coder.get("write_file").is_some());
        assert!(coder.get("exec").is_some(), "coder keeps shell by default");
        let noexec = SubagentKind::subagent_registry(jail.clone(), &skills, &[], SubagentKind::Coder, false);
        assert!(noexec.get("exec").is_none(), "Tools-page opt-out removes shell everywhere");
        let researcher =
            SubagentKind::subagent_registry(jail, &skills, &[], SubagentKind::Researcher, true);
        assert!(researcher.get("skill").is_some(), "researcher reads skills too");
        assert!(researcher.get("write_file").is_none(), "researcher must be read-only");
        assert!(researcher.get("spawn_subagent").is_none());
        assert!(SubagentKind::Coder.prompt(&skills).contains("acme-deploy"));
        assert!(!SubagentKind::Coder.prompt(&[]).contains("acme-deploy"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    struct DenyGate;
    impl ApprovalGate for DenyGate {
        fn decide(&self, _req: ApprovalRequest) -> Pin<Box<dyn Future<Output = Approved> + Send>> {
            Box::pin(async { Approved::Denied })
        }
    }

    fn test_run(jail: Arc<crate::sandbox::PathJail>, vision: bool) -> AgentRun<'static> {
        // Leaked client never touches the network (read_file is local).
        let client: &'static LlmClient = Box::leak(Box::new(LlmClient::new("http://127.0.0.1:9")));
        AgentRun {
            client,
            registry: Arc::new(ToolRegistry::project_tools(jail)),
            engine: Arc::new(PermissionEngine::new()),
            model: None,
            project: Some("p".to_string()),
            reasoning_effort: None,
            max_turns: 5,
            subagents: None,
            vision,
            context_limit: None,
            compactions_log: Arc::new(Mutex::new(Vec::new())),
        }
    }

    #[test]
    fn image_media_only_reaches_vision_runners() {
        let dir = std::env::temp_dir().join(format!("harness-vision-gate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut png = vec![0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        png.extend([0u8; 64]);
        std::fs::write(dir.join("art.png"), &png).unwrap();
        let jail = Arc::new(crate::sandbox::PathJail::new(&dir, &[], &[]).unwrap());
        let gate: Arc<dyn ApprovalGate> = Arc::new(DenyGate);
        let args = serde_json::json!({"path": "art.png"});
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        // Vision runner: pixels ride along.
        let run = test_run(jail.clone(), true);
        let (text, media) = rt.block_on(async {
            let mut noop = |_ev: AgentEvent| {};
            run.execute_tool_call("read_file", &args, String::new(), "c1", &gate, &mut noop).await
        });
        assert!(text.contains("read for visual inspection"), "{text}");
        assert_eq!(media.len(), 1);
        // Text-only runner: pixels withheld, delegation hint instead — the
        // request would 500 server-side otherwise.
        let run = test_run(jail, false);
        let (text, media) = rt.block_on(async {
            let mut noop = |_ev: AgentEvent| {};
            run.execute_tool_call("read_file", &args, String::new(), "c1", &gate, &mut noop).await
        });
        assert!(text.contains("no vision"), "{text}");
        assert!(media.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
