//! Declarative agent definitions: markdown files with YAML-ish frontmatter
//! frontmatter (`name`, `description`, `tools`, `model`) whose body is the
//! system prompt. Discovered fresh on every spawn so edits take effect
//! mid-session; a malformed file is skipped, never fatal.

use std::path::PathBuf;

/// One discovered agent definition.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentDef {
    pub name: String,
    pub description: String,
    /// Tool allowlist (empty = the harness defaults for spawned agents).
    pub tools: Vec<String>,
    /// Model override (path for local GGUF); None inherits the worker role.
    pub model: Option<String>,
    /// Body of the file = the agent's system prompt.
    pub prompt: String,
    /// Folder the file came from ("global" | "project"); project wins on name.
    pub scope: String,
}

/// Discover agent definitions across the given roots; later roots override
/// earlier ones on name conflicts (project beats global).
pub fn discover(roots: &[PathBuf]) -> Vec<AgentDef> {
    let mut out: Vec<AgentDef> = Vec::new();
    for (root_idx, root) in roots.iter().enumerate() {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let file_stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            let Some(mut def) = parse_agent_md(&content, &file_stem) else {
                continue;
            };
            def.scope = if root_idx == 0 && roots.len() > 1 { "global" } else { "project" }.into();
            out.retain(|d: &AgentDef| d.name != def.name);
            out.push(def);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Parse frontmatter `name`/`description`/`tools`/`model`; the body (after the
/// closing `---`) is the prompt. Returns None when the file has no usable
/// frontmatter block.
pub fn parse_agent_md(content: &str, fallback_name: &str) -> Option<AgentDef> {
    let mut lines = content.lines().peekable();
    if lines.peek().map(|l| l.trim()) != Some("---") {
        return None;
    }
    lines.next();
    let mut name = String::new();
    let mut description = String::new();
    let mut tools: Vec<String> = Vec::new();
    let mut model: Option<String> = None;
    let mut body_started = false;
    let mut body = String::new();
    for line in lines {
        if line.trim() == "---" {
            body_started = true;
            continue;
        }
        if body_started {
            body.push_str(line);
            body.push('\n');
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "name" => name = value.trim_matches('"').to_string(),
            "description" => description = value.trim_matches('"').to_string(),
            "model" => {
                let m = value.trim_matches('"');
                if !m.is_empty() {
                    model = Some(m.to_string());
                }
            }
            "tools" => {
                tools = value
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .split(',')
                    .map(|t| t.trim().trim_matches('"').to_string())
                    .filter(|t| !t.is_empty())
                    .collect();
            }
            _ => {}
        }
    }
    if !body_started || (name.is_empty() && fallback_name.is_empty()) {
        return None;
    }
    if name.is_empty() {
        name = fallback_name.to_string();
    }
    Some(AgentDef { name, description, tools, model, prompt: body.trim().to_string(), scope: String::new() })
}

/// Optional `tools` gate: restricts the registry for this spawn. Mirrors the
/// kind allowlists — unknown names are dropped by the caller.
pub fn agent_def_prompt(def: &AgentDef) -> String {
    def.prompt.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frontmatter_and_body() {
        let md = "---\nname: scout\ndescription: Explores the codebase\ntools: read, find\nmodel: E:\\models\\small.gguf\n---\nYou are a scout. Be terse.";
        let def = parse_agent_md(md, "scout").unwrap();
        assert_eq!(def.name, "scout");
        assert_eq!(def.description, "Explores the codebase");
        assert_eq!(def.tools, vec!["read", "find"]);
        assert_eq!(def.model.as_deref(), Some("E:\\models\\small.gguf"));
        assert_eq!(def.prompt, "You are a scout. Be terse.");
    }

    #[test]
    fn tools_array_form_and_defaults() {
        let md = "---\nname: a\ntools: [read, write_file]\n---\nprompt";
        let def = parse_agent_md(md, "a").unwrap();
        assert_eq!(def.tools, vec!["read", "write_file"]);
        assert_eq!(def.model, None);
    }

    #[test]
    fn rejects_missing_frontmatter_and_falls_back_name() {
        assert!(parse_agent_md("just text", "x").is_none());
        let md = "---\ndescription: d\n---\nprompt";
        let def = parse_agent_md(md, "fallback").unwrap();
        assert_eq!(def.name, "fallback");
    }

    #[test]
    fn discover_project_overrides_global() {
        let dir = std::env::temp_dir().join(format!("harness-agents-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let global = dir.join("global");
        let project = dir.join("project");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            global.join("alpha.md"),
            "---\nname: alpha\ndescription: g\n---\nglobal prompt",
        )
        .unwrap();
        std::fs::write(
            global.join("beta.md"),
            "---\nname: beta\ndescription: b\n---\nbeta prompt",
        )
        .unwrap();
        std::fs::write(
            project.join("alpha.md"),
            "---\nname: alpha\ndescription: p\ntools: read\n---\nproject prompt",
        )
        .unwrap();
        std::fs::write(project.join("broken.md"), "no frontmatter").unwrap();
        let defs = discover(&[global, project]);
        assert_eq!(defs.len(), 2, "broken skipped, alpha deduped: {defs:?}");
        let alpha = defs.iter().find(|d| d.name == "alpha").unwrap();
        assert_eq!(alpha.prompt, "project prompt");
        assert!(defs.iter().any(|d| d.name == "beta"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_agent_file_never_fails_discovery() {
        let dir = std::env::temp_dir().join(format!("harness-agents-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("x.md"), "garbage without frontmatter").unwrap();
        std::fs::write(dir.join("y.md"), "---\nname: ok\n---\nprompt").unwrap();
        let defs = discover(&[dir.clone()]);
        assert_eq!(defs.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
