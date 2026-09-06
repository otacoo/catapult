//! Sandboxed tool implementations for the harness.
//!
//! Every path-taking tool resolves through [`PathJail`] before touching the
//! filesystem — the model can never escape the project by traversal or
//! symlinks. `edit_file` is an exact-match search/replace with loud failure:
//! zero or multiple matches are errors, never silent corruption.
//!
//! Approval: read-only tools return `None` from `approval_key` (auto-approved);
//! mutating tools return an [`ApprovalKey`] that the orchestrator checks
//! against the [`PermissionEngine`].

use std::sync::Arc;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::permissions::{ApprovalKey, PermissionEngine};
use crate::sandbox::PathJail;

/// Max characters returned by file-reading tools before truncation.
const READ_CAP: usize = 100_000;
/// Max matches returned by `search_content`.
const SEARCH_CAP: usize = 50;
/// Max entries returned by `find_files`.
const FIND_CAP: usize = 100;
/// Max chars captured from command output.
const EXEC_CAP: usize = 50_000;
/// Shell command timeout (poll-based, cooperative).
const EXEC_TIMEOUT_SECS: u64 = 120;

pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    /// OpenAI function-calling JSON schema for the parameters.
    fn parameters(&self) -> Value;
    /// `None` = auto-approved (read-only). `Some(key)` = consult the
    /// permission engine before executing.
    fn approval_key(&self, args: &Value) -> Option<ApprovalKey>;
    fn execute(&self, args: &Value) -> Result<String>;
}

fn str_arg(args: &Value, key: &str) -> Result<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .with_context(|| format!("missing string argument '{key}'"))
}

/// Directories never searched or listed (noise / build output).
fn ignored_dir(name: &str) -> bool {
    matches!(
        name,
        ".git" | "node_modules" | "target" | ".venv" | "__pycache__" | "dist" | ".catapult"
    )
}

// ── read_file ───────────────────────────────────────────────────────────────

pub struct ReadFileTool {
    jail: Arc<PathJail>,
}

impl Tool for ReadFileTool {
    fn name(&self) -> &'static str {
        "read_file"
    }
    fn description(&self) -> &'static str {
        "Read a text file inside the project. Large files are truncated."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path relative to the project root (or an allowlisted read path)" }
            },
            "required": ["path"]
        })
    }
    fn approval_key(&self, _args: &Value) -> Option<ApprovalKey> {
        None // read-only
    }
    fn execute(&self, args: &Value) -> Result<String> {
        let path = str_arg(args, "path")?;
        let resolved = self.jail.check_read(std::path::Path::new(&path))?;
        let text = std::fs::read_to_string(&resolved)
            .with_context(|| format!("Cannot read {}", resolved.display()))?;
        if text.chars().count() > READ_CAP {
            let truncated: String = text.chars().take(READ_CAP).collect();
            Ok(format!(
                "{}\n\n[truncated — file has {} more characters; read a narrower region if needed]",
                truncated,
                text.chars().count() - READ_CAP
            ))
        } else {
            Ok(text)
        }
    }
}

// ── write_file ──────────────────────────────────────────────────────────────

pub struct WriteFileTool {
    jail: Arc<PathJail>,
}

impl Tool for WriteFileTool {
    fn name(&self) -> &'static str {
        "write_file"
    }
    fn description(&self) -> &'static str {
        "Create or overwrite a text file inside the project."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path relative to the project root" },
                "content": { "type": "string" }
            },
            "required": ["path", "content"]
        })
    }
    fn approval_key(&self, _args: &Value) -> Option<ApprovalKey> {
        Some(ApprovalKey { tool: "write_file".into(), command: None })
    }
    fn execute(&self, args: &Value) -> Result<String> {
        let path = str_arg(args, "path")?;
        let content = args
            .get("content")
            .and_then(|c| c.as_str())
            .context("missing string argument 'content'")?;
        let resolved = self.jail.check_write(std::path::Path::new(&path))?;
        if let Some(parent) = resolved.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Cannot create directory {}", parent.display()))?;
        }
        std::fs::write(&resolved, content)
            .with_context(|| format!("Cannot write {}", resolved.display()))?;
        Ok(format!("Wrote {} bytes to {}", content.len(), resolved.display()))
    }
}

// ── edit_file ───────────────────────────────────────────────────────────────

pub struct EditFileTool {
    jail: Arc<PathJail>,
}

impl Tool for EditFileTool {
    fn name(&self) -> &'static str {
        "edit_file"
    }
    fn description(&self) -> &'static str {
        "Exact-match search/replace in a project file. The search text must appear exactly once — include enough surrounding context to disambiguate. Fails loudly on zero or multiple matches."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "search": { "type": "string", "description": "Exact text to replace (must match exactly once)" },
                "replace": { "type": "string" }
            },
            "required": ["path", "search", "replace"]
        })
    }
    fn approval_key(&self, _args: &Value) -> Option<ApprovalKey> {
        Some(ApprovalKey { tool: "edit_file".into(), command: None })
    }
    fn execute(&self, args: &Value) -> Result<String> {
        let path = str_arg(args, "path")?;
        let search = str_arg(args, "search")?;
        let replace = str_arg(args, "replace")?;
        if search.is_empty() {
            bail!("'search' must not be empty");
        }
        let resolved = self.jail.check_write(std::path::Path::new(&path))?;
        let text = std::fs::read_to_string(&resolved)
            .with_context(|| format!("Cannot read {}", resolved.display()))?;
        let matches = text.matches(&search).count();
        match matches {
            0 => bail!(
                "edit_file: search text not found in {} — re-read the file and use exact content",
                path
            ),
            1 => {
                let updated = text.replacen(&search, &replace, 1);
                std::fs::write(&resolved, &updated)
                    .with_context(|| format!("Cannot write {}", resolved.display()))?;
                Ok(format!("Edited {} (1 match replaced)", resolved.display()))
            }
            n => bail!(
                "edit_file: search text matches {n} places in {} — include more surrounding context",
                path
            ),
        }
    }
}

// ── find_files ──────────────────────────────────────────────────────────────

pub struct FindFilesTool {
    jail: Arc<PathJail>,
}

impl Tool for FindFilesTool {
    fn name(&self) -> &'static str {
        "find_files"
    }
    fn description(&self) -> &'static str {
        "List project files matching a glob pattern (e.g. 'src/**/*.rs'). Respects standard ignores (.git, node_modules, target, …)."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob pattern relative to the project root" }
            },
            "required": ["pattern"]
        })
    }
    fn approval_key(&self, _args: &Value) -> Option<ApprovalKey> {
        None
    }
    fn execute(&self, args: &Value) -> Result<String> {
        let pattern = str_arg(args, "pattern")?;
        let root = self.jail.root();
        let full = root.join(&pattern);
        let pattern_str = full.to_string_lossy().replace('\\', "/");
        let mut found: Vec<String> = glob::glob(&pattern_str)
            .map_err(|e| anyhow::anyhow!("Invalid glob '{}': {e}", pattern_str))?
            .filter_map(|p| p.ok())
            .take(FIND_CAP)
            .map(|p| {
                p.strip_prefix(root)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        let truncated = found.len() == FIND_CAP;
        if found.is_empty() {
            return Ok(format!("No files match '{pattern}'."));
        }
        if truncated {
            found.push(format!("[truncated at {FIND_CAP} entries]"));
        }
        Ok(found.join("\n"))
    }
}

// ── search_content ──────────────────────────────────────────────────────────

pub struct SearchContentTool {
    jail: Arc<PathJail>,
}

impl Tool for SearchContentTool {
    fn name(&self) -> &'static str {
        "search_content"
    }
    fn description(&self) -> &'static str {
        "Regex search over project text files. Returns path:line: match, capped. Respects standard ignores."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Regular expression" },
                "pattern": { "type": "string", "description": "Optional glob to restrict files, e.g. 'src/**/*.ts'" }
            },
            "required": ["query"]
        })
    }
    fn approval_key(&self, _args: &Value) -> Option<ApprovalKey> {
        None
    }
    fn execute(&self, args: &Value) -> Result<String> {
        let query = str_arg(args, "query")?;
        let filter = args.get("pattern").and_then(|p| p.as_str()).unwrap_or("**/*");
        let re = regex::Regex::new(&query).map_err(|e| anyhow::anyhow!("Invalid regex: {e}"))?;
        let root = self.jail.root();
        let matcher = glob::Pattern::new(&root.join(filter).to_string_lossy().replace('\\', "/"))
            .map_err(|e| anyhow::anyhow!("Invalid glob '{filter}': {e}"))?;

        let mut out: Vec<String> = Vec::new();
        let mut truncated = false;
        for entry in walkdir::WalkDir::new(root)
            .into_iter()
            .filter_entry(|e| {
                e.file_type().is_file()
                    || e.file_name()
                        .to_str()
                        .map(|n| !ignored_dir(n))
                        .unwrap_or(true)
            })
        {
            let entry = entry?;
            if !entry.file_type().is_file() {
                continue;
            }
            let rel = entry.path().strip_prefix(root).unwrap_or(entry.path());
            let rel_norm = rel.to_string_lossy().replace('\\', "/");
            let full_norm = entry.path().to_string_lossy().replace('\\', "/");
            if !matcher.matches(&full_norm) {
                continue;
            }
            // Skip files over ~1MB — likely binaries or generated blobs.
            if entry.metadata().map(|m| m.len() > 2_000_000).unwrap_or(true) {
                continue;
            }
            let text = match std::fs::read_to_string(entry.path()) {
                Ok(t) => t,
                Err(_) => continue, // binary or unreadable
            };
            for (lineno, line) in text.lines().enumerate() {
                if re.is_match(line) {
                    out.push(format!("{}:{}: {}", rel_norm, lineno + 1, line.trim()));
                    if out.len() >= SEARCH_CAP {
                        truncated = true;
                        break;
                    }
                }
            }
            if truncated {
                break;
            }
        }
        if out.is_empty() {
            return Ok(format!("No matches for '{query}'."));
        }
        if truncated {
            out.push(format!("[truncated at {SEARCH_CAP} matches]"));
        }
        Ok(out.join("\n"))
    }
}

// ── exec (shell) ────────────────────────────────────────────────────────────

/// Head tokens auto-approved as read-only (late's "safe commands"). Anything
/// else goes through the approval prompt; grants are scoped per head token.
const READONLY_COMMANDS: &[&str] = &[
    "dir", "ls", "pwd", "type", "cat", "get-content", "get-childitem", "get-location",
    "head", "tail", "wc", "where", "which", "select-string", "grep", "findstr",
    "git status", "git log", "git diff", "git show", "git branch", "git remote",
];

/// Does the command head (plus optional subcommand for git-like tools) match
/// the read-only allowlist?
fn command_is_readonly(command: &str) -> bool {
    let lower = command.trim().to_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();
    if words.is_empty() {
        return false;
    }
    let two_words = words.iter().take(2).cloned().collect::<Vec<_>>().join(" ");
    READONLY_COMMANDS
        .iter()
        .any(|c| *c == words[0] || *c == two_words)
}

pub struct ExecTool {
    jail: Arc<PathJail>,
}

impl Tool for ExecTool {
    fn name(&self) -> &'static str {
        "exec"
    }
    fn description(&self) -> &'static str {
        "Run a shell command in the project directory. Read-only commands (dir, cat, git status, …) run automatically; anything else requires approval."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string" }
            },
            "required": ["command"]
        })
    }
    fn approval_key(&self, args: &Value) -> Option<ApprovalKey> {
        let command = args.get("command").and_then(|c| c.as_str()).unwrap_or("");
        if command_is_readonly(command) {
            None
        } else {
            let head = command
                .split_whitespace()
                .next()
                .unwrap_or("unknown")
                .to_lowercase();
            Some(ApprovalKey { tool: "exec".into(), command: Some(head) })
        }
    }
    fn execute(&self, args: &Value) -> Result<String> {
        let command = str_arg(args, "command")?;
        if command.trim().is_empty() {
            bail!("'command' must not be empty");
        }
        #[cfg(target_os = "windows")]
        let mut cmd = {
            let mut c = std::process::Command::new("powershell");
            c.args(["-NoLogo", "-NoProfile", "-Command", &command]);
            c
        };
        #[cfg(not(target_os = "windows"))]
        let mut cmd = {
            let mut c = std::process::Command::new("sh");
            c.arg("-c").arg(&command);
            c
        };
        cmd.current_dir(self.jail.root())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        #[cfg(target_os = "windows")]
        {
            #[allow(unused_imports)]
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }
        let mut child = cmd
            .spawn()
            .with_context(|| format!("Failed to spawn command: {command}"))?;
        let stdout = child.stdout.take().context("no stdout pipe")?;
        let stderr = child.stderr.take().context("no stderr pipe")?;
        // Read stdout/stderr on threads; pipes close when the child exits.
        let out_handle = std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = Vec::new();
            let mut out = std::io::BufReader::new(stdout);
            let _ = out.read_to_end(&mut buf);
            buf
        });
        let err_handle = std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = Vec::new();
            let mut err = std::io::BufReader::new(stderr);
            let _ = err.read_to_end(&mut buf);
            buf
        });
        // Poll-based wait with timeout so a hung command can be killed.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(EXEC_TIMEOUT_SECS);
        let mut status: Option<std::process::ExitStatus> = None;
        let mut timed_out = false;
        loop {
            match child.try_wait() {
                Ok(Some(s)) => {
                    status = Some(s);
                    break;
                }
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        timed_out = true;
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(e) => bail!("Failed to wait for command: {e}"),
            }
        }
        let out_bytes = out_handle.join().unwrap_or_default();
        let err_bytes = err_handle.join().unwrap_or_default();
        let mut text = String::from_utf8_lossy(&out_bytes).to_string();
        let err_text = String::from_utf8_lossy(&err_bytes).trim().to_string();
        if !err_text.is_empty() {
            text.push_str("\n[stderr]\n");
            text.push_str(&err_text);
        }
        if text.chars().count() > EXEC_CAP {
            let n = text.chars().count();
            text = text.chars().take(EXEC_CAP).collect();
            text.push_str(&format!("\n[truncated — {} more characters]", n - EXEC_CAP));
        }
        if timed_out {
            text.push_str(&format!("\n[command timed out after {EXEC_TIMEOUT_SECS}s and was killed]"));
        } else if let Some(s) = &status {
            if !s.success() {
                text.push_str(&format!("\n[exit code {}]", s.code().unwrap_or(-1)));
            }
        }
        Ok(text)
    }
}

// ── Registry ────────────────────────────────────────────────────────────────

/// Registry of sandboxed tools rooted at one project jail.
pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
}

impl ToolRegistry {
    /// Standard project toolset. Shell (`exec`) is included; approvals are
    /// decided per-call via the permission engine.
    pub fn project_tools(jail: Arc<PathJail>) -> Self {
        Self {
            tools: vec![
                Box::new(ReadFileTool { jail: jail.clone() }),
                Box::new(WriteFileTool { jail: jail.clone() }),
                Box::new(EditFileTool { jail: jail.clone() }),
                Box::new(FindFilesTool { jail: jail.clone() }),
                Box::new(SearchContentTool { jail: jail.clone() }),
                Box::new(ExecTool { jail }),
            ],
        }
    }

    /// Drop tools by name (used to strip `spawn_subagent` from subagents later).
    pub fn without(self, names: &[&str]) -> Self {
        Self {
            tools: self
                .tools
                .into_iter()
                .filter(|t| !names.contains(&t.name()))
                .collect(),
        }
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.iter().find(|t| t.name() == name).map(|t| &**t)
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.tools.iter().map(|t| t.name()).collect()
    }

    /// OpenAI `tools` array entries for the chat request.
    pub fn tool_schemas(&self) -> Vec<Value> {
        self.tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name(),
                        "description": t.description(),
                        "parameters": t.parameters(),
                    }
                })
            })
            .collect()
    }

    /// Should this call be executed now? Read-only → yes; otherwise consult
    /// the permission engine with the tool's approval key.
    pub fn check_permissions(&self, name: &str, args: &Value, engine: &PermissionEngine) -> bool {
        match self.get(name) {
            None => false, // unknown tool → refuse
            Some(tool) => match tool.approval_key(args) {
                None => true,
                Some(key) => engine.check(&key) == crate::permissions::Decision::Allowed,
            },
        }
    }

    /// Run a tool on the blocking thread pool (file/process work must not
    /// stall the async runtime). Caller holds `Arc<Self>` because tools are
    /// not `Clone`.
    pub fn spawn_execute(
        self: &Arc<Self>,
        name: String,
        args: Value,
    ) -> tokio::task::JoinHandle<Result<String>> {
        let registry = self.clone();
        tokio::task::spawn_blocking(move || {
            match registry.get(&name) {
                Some(t) => t.execute(&args),
                None => Err(anyhow::anyhow!("Unknown tool '{name}'")),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::PathJail;

    fn temp_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("harness-tools-{}-{}", label, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn registry_for(root: &std::path::Path) -> ToolRegistry {
        let jail = Arc::new(PathJail::new(root, &[], &[]).unwrap());
        ToolRegistry::project_tools(jail)
    }

    #[test]
    fn write_then_read_roundtrip() {
        let root = temp_dir("roundtrip");
        let reg = registry_for(&root);
        let out = reg
            .get("write_file")
            .unwrap()
            .execute(&json!({"path": "docs/a.txt", "content": "hello"}))
            .unwrap();
        assert!(out.contains("Wrote 5 bytes"));
        let read = reg
            .get("read_file")
            .unwrap()
            .execute(&json!({"path": "docs/a.txt"}))
            .unwrap();
        assert_eq!(read, "hello");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn write_outside_jail_denied() {
        let root = temp_dir("write-deny");
        let registry = registry_for(&root);
        let err = registry
            .get("write_file")
            .unwrap()
            .execute(&json!({"path": "../escape.txt", "content": "x"}));
        assert!(err.is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn edit_file_requires_exact_single_match() {
        let root = temp_dir("edit");
        let registry = registry_for(&root);
        std::fs::write(root.join("a.rs"), "fn main() {\n    println!(\"hi\");\n}\n").unwrap();
        let edit = registry.get("edit_file").unwrap();

        // Zero matches fail loud.
        assert!(edit.execute(&json!({"path": "a.rs", "search": "NOPE", "replace": "x"})).is_err());
        // Two matches fail.
        std::fs::write(root.join("b.rs"), "same\nsame\n").unwrap();
        assert!(edit.execute(&json!({"path": "b.rs", "search": "same", "replace": "x"})).is_err());
        // Single match replaces.
        edit.execute(&json!({"path": "a.rs", "search": "println!(\"hi\");", "replace": "println!(\"bye\");"})).unwrap();
        let text = std::fs::read_to_string(root.join("a.rs")).unwrap();
        assert!(text.contains("bye") || text.contains("hello") || !text.contains("hi"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn find_files_respects_ignores_and_pattern() {
        let root = temp_dir("find");
        std::fs::create_dir_all(root.join("src/deep")).unwrap();
        std::fs::write(root.join("src/deep/lib.rs"), "").unwrap();
        std::fs::write(root.join("src/main.rs"), "").unwrap();
        std::fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        std::fs::write(root.join("node_modules/pkg/index.js"), "").unwrap();
        let registry = registry_for(&root);
        let out = registry
            .get("find_files")
            .unwrap()
            .execute(&json!({"pattern": "src/**/*.rs"}))
            .unwrap();
        assert!(out.contains("src/deep/lib.rs"));
        assert!(out.contains("src/main.rs"));
        assert!(!out.contains("node_modules"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn search_content_finds_regex_matches() {
        let root = temp_dir("search");
        std::fs::write(root.join("a.txt"), "first line\nneedle here\nthird\n").unwrap();
        let registry = registry_for(&root);
        let out = registry
            .get("search_content")
            .unwrap()
            .execute(&json!({"query": "needle", "pattern": "**/*.txt"}))
            .unwrap();
        assert!(out.contains("a.txt:2: needle here"), "got: {out}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn exec_allowlisted_runs_without_approval() {
        let root = temp_dir("exec-allow");
        let registry = registry_for(&root);
        let exec = registry.get("exec").unwrap();
        let cmd = if cfg!(windows) { "Get-Location" } else { "pwd" };
        let args = json!({"command": cmd});
        assert!(exec.approval_key(&args).is_none(), "read-only must auto-approve");
        let out = exec.execute(&args).unwrap();
        assert!(out.contains("Path") || out.to_lowercase().contains(&root.to_string_lossy().to_lowercase()));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn exec_nonallowlisted_requires_approval_per_head() {
        let root = temp_dir("exec-approval");
        let registry = registry_for(&root);
        let exec = registry.get("exec").unwrap();
        let args = json!({"command": "npm install left-pad"});
        let key = exec.approval_key(&args).expect("must need approval");
        assert_eq!(key.tool, "exec");
        assert_eq!(key.command.as_deref(), Some("npm"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn registry_permissions_gate_mutating_tools() {
        let root = temp_dir("perm-gate");
        let registry = registry_for(&root);
        let engine = crate::permissions::PermissionEngine::new();
        // read_file auto-allowed
        assert!(registry.check_permissions("read_file", &json!({"path": "x"}), &engine));
        // write needs a grant
        assert!(!registry.check_permissions("write_file", &json!({"path": "x", "content": ""}), &engine));
        engine.grant(crate::permissions::Grant {
            tool: "write_file".into(),
            command: None,
            scope: crate::permissions::Scope::Once,
            expires: None,
        });
        assert!(registry.check_permissions("write_file", &json!({"path": "x", "content": ""}), &engine));
        // Consumed once-grant
        assert!(!registry.check_permissions("write_file", &json!({"path": "x", "content": ""}), &engine));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn tool_schemas_have_openai_shape() {
        let root = temp_dir("schema");
        let registry = registry_for(&root);
        let schemas = registry.tool_schemas();
        assert_eq!(schemas.len(), registry.names().len());
        for s in schemas {
            assert_eq!(s["type"], "function");
            assert!(s["function"]["name"].is_string());
            assert!(s["function"]["parameters"].is_object());
        }
        std::fs::remove_dir_all(&root).unwrap();
    }
}
