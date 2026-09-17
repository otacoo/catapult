//! Declarative long-term memory (MEMORY.md): global + per-project files injected as one stable system-prompt block.
//! No retrieval layer (files stay small); contrast skills (on-demand procedures) and sessions (episodic history).

use anyhow::{Context, Result};
use serde_json::{json, Value};

pub const MEMORY_BLOCK_CAP: usize = 4 * 1024;
/// Refuse new notes past this file size (forget something first).
pub const MEMORY_FILE_CAP: usize = 16 * 1024;

pub fn project_memory_path(root: &std::path::Path) -> std::path::PathBuf {
    root.join(".catapult").join("MEMORY.md")
}

/// Combined block (global first); missing files contribute nothing. Capped to stay cache-friendly.
pub fn load_block(global_file: Option<&std::path::Path>, project_root: &std::path::Path) -> String {
    let mut sections = Vec::new();
    if let Some(path) = global_file {
        if let Some(text) = read_trimmed(path) {
            sections.push(format!("User memory:\n{text}"));
        }
    }
    let project_file = project_memory_path(project_root);
    if let Some(text) = read_trimmed(&project_file) {
        sections.push(format!("Project memory:\n{text}"));
    }
    if sections.is_empty() {
        return String::new();
    }
    let mut block = format!("\n\nMemory:\n{}", sections.join("\n\n"));
    if block.chars().count() > MEMORY_BLOCK_CAP {
        let truncated: String = block.chars().take(MEMORY_BLOCK_CAP).collect();
        block = format!("{truncated}\n[…memory truncated]");
    }
    block
}

fn read_trimmed(path: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let text = text.trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Curator tool; writes are approval-gated, `scope` picks the file.
pub struct RememberTool {
    project_file: std::path::PathBuf,
    global_file: Option<std::path::PathBuf>,
}

impl RememberTool {
    pub fn new(project_root: &std::path::Path, global_file: Option<std::path::PathBuf>) -> Self {
        Self {
            project_file: project_memory_path(project_root),
            global_file,
        }
    }

    fn target(&self, scope: &str) -> Result<std::path::PathBuf> {
        match scope {
            "global" => self.global_file.clone().context("No global memory file configured"),
            _ => Ok(self.project_file.clone()),
        }
    }
}

impl crate::tools::Tool for RememberTool {
    fn name(&self) -> String {
        "remember".to_string()
    }

    fn description(&self) -> String {
        "Save or remove durable memories (user preferences, project conventions, corrections that should stick across sessions). Memories are injected into every run's system prompt — keep them short. Scope 'project' (default) or 'global'.".to_string()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["note", "forget", "show"], "description": "note: append a fact; forget: remove lines containing text; show: read a memory file" },
                "text": { "type": "string", "description": "The fact (note), substring to remove (forget), ignored for show" },
                "scope": { "type": "string", "enum": ["project", "global"], "description": "Which memory file (default: project)" }
            },
            "required": ["action"]
        })
    }

    fn approval_key(&self, args: &Value) -> Option<crate::permissions::ApprovalKey> {
        // Reading memory is free; persisting knowledge is a deliberate act
        // that goes through the approval prompt like any other write.
        if args.get("action").and_then(|a| a.as_str()) == Some("show") {
            None
        } else {
            Some(crate::permissions::ApprovalKey { tool: "remember".into(), command: None })
        }
    }

    fn execute(&self, args: &Value) -> Result<String> {
        let action = args.get("action").and_then(|a| a.as_str()).unwrap_or("");
        let scope = args.get("scope").and_then(|s| s.as_str()).unwrap_or("project");
        let path = self.target(scope)?;
        match action {
            "show" => Ok(read_trimmed(&path).unwrap_or_else(|| "(empty)".to_string())),
            "note" => {
                let fact = args.get("text").and_then(|t| t.as_str()).unwrap_or("").trim();
                if fact.is_empty() {
                    anyhow::bail!("'text' must not be empty for action 'note'");
                }
                if fact.chars().count() > 2000 {
                    anyhow::bail!("Fact too long (2000 chars max) — keep memories short");
                }
                let existing = std::fs::read_to_string(&path).unwrap_or_default();
                if existing.len() + fact.len() > MEMORY_FILE_CAP {
                    anyhow::bail!("Memory file is full — 'forget' something first");
                }
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)
                        .with_context(|| format!("Cannot create directory {}", parent.display()))?;
                }
                let mut out = existing;
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push_str(&format!("- {fact}\n"));
                std::fs::write(&path, &out)
                    .with_context(|| format!("Cannot write {}", path.display()))?;
                Ok(format!("Noted in {} memory", scope))
            }
            "forget" => {
                let pattern = args.get("text").and_then(|t| t.as_str()).unwrap_or("").trim();
                if pattern.is_empty() {
                    anyhow::bail!("'text' must not be empty for action 'forget'");
                }
                let existing = std::fs::read_to_string(&path).unwrap_or_default();
                let needle = pattern.to_lowercase();
                let kept: Vec<&str> = existing
                    .lines()
                    .filter(|l| !l.to_lowercase().contains(&needle))
                    .collect();
                let removed = existing.lines().count() - kept.len();
                let mut out = kept.join("\n");
                if !out.is_empty() {
                    out.push('\n');
                }
                std::fs::write(&path, &out)
                    .with_context(|| format!("Cannot write {}", path.display()))?;
                Ok(format!("Forgot {removed} line(s) from {} memory", scope))
            }
            _ => anyhow::bail!("Unknown action '{action}' (expected note, forget, or show)"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Tool as _;

    fn setup(label: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("harness-memory-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let global = dir.join("global_MEMORY.md");
        (dir, global)
    }

    #[test]
    fn note_forget_show_roundtrip() {
        let (root, global) = setup("roundtrip");
        let tool = RememberTool::new(&root, Some(global.clone()));
        assert!(tool.approval_key(&json!({"action": "show"})).is_none());
        assert!(tool.approval_key(&json!({"action": "note", "text": "x"})).is_some());
        let out = tool.execute(&json!({"action": "show"})).unwrap();
        assert_eq!(out, "(empty)");
        tool.execute(&json!({"action": "note", "text": "Always use pnpm here"})).unwrap();
        let out = tool.execute(&json!({"action": "show"})).unwrap();
        assert!(out.contains("pnpm"));
        tool.execute(&json!({"action": "note", "text": "My name is Ada", "scope": "global"})).unwrap();
        let out = tool.execute(&json!({"action": "show", "scope": "global"})).unwrap();
        assert!(out.contains("Ada"));
        let out = tool.execute(&json!({"action": "show"})).unwrap();
        assert!(!out.contains("Ada"));
        let out = tool.execute(&json!({"action": "forget", "text": "pnpm"})).unwrap();
        assert!(out.contains('1'));
        let out = tool.execute(&json!({"action": "show"})).unwrap();
        assert_eq!(out, "(empty)");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn load_block_combines_and_caps() {
        let (root, global) = setup("block");
        std::fs::write(&global, "global fact").unwrap();
        let tool = RememberTool::new(&root, Some(global));
        tool.execute(&json!({"action": "note", "text": "project fact"})).unwrap();
        let block = load_block(tool.global_file.as_deref(), &root);
        assert!(block.contains("User memory"));
        assert!(block.contains("Project memory"));
        assert!(block.contains("global fact"));
        assert!(block.contains("project fact"));
        let empty = load_block(None, &std::env::temp_dir().join("harness-memory-nope"));
        assert_eq!(empty, "");
        let _ = std::fs::remove_dir_all(&root);
    }
}
