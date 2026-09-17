//! Agent Skills discovery (`SKILL.md` folders): progressive disclosure — only names + descriptions reach the system prompt.
//! The `skill` tool loads full instructions on demand.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct Skill {
    /// Name from frontmatter (falls back to the folder name).
    pub name: String,
    /// Short description from frontmatter (shown in the system prompt).
    pub description: String,
    pub path: PathBuf,
}

/// Parse frontmatter `name`/`description`, tolerating missing fields.
fn parse_skill_md(content: &str, folder: &str) -> (String, String) {
    let mut name = folder.to_string();
    let mut description = String::new();
    let mut in_front = false;
    for line in content.lines() {
        match line.trim() {
            "---" => {
                if in_front {
                    break;
                }
                if name != folder || !description.is_empty() {
                    break;
                }
                in_front = true;
                continue;
            }
            other if in_front => {
                if let Some(v) = other.strip_prefix("name:") {
                    name = v.trim().trim_matches('"').to_string();
                } else if let Some(v) = other.strip_prefix("description:") {
                    description = v.trim().trim_matches('"').to_string();
                }
            }
            _ => break, // body reached without a frontmatter block
        }
    }
    if name.is_empty() {
        name = folder.to_string();
    }
    (name, description)
}

/// Discover skills: one `SKILL.md` per subfolder in any of the given roots.
/// Later roots override earlier ones on name conflicts (project beats global).
pub fn discover(roots: &[PathBuf]) -> Vec<Skill> {
    let mut out: Vec<Skill> = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let skill_file = dir.join("SKILL.md");
            let Ok(content) = std::fs::read_to_string(&skill_file) else {
                continue;
            };
            let folder = dir.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
            let (name, description) = parse_skill_md(&content, &folder);
            out.retain(|s: &Skill| s.name != name);
            out.push(Skill { name, description, path: skill_file });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

pub fn load(skill: &Skill) -> Result<String> {
    std::fs::read_to_string(&skill.path)
        .with_context(|| format!("Cannot read skill {}", skill.path.display()))
}

pub struct SkillTool {
    skills: Vec<Skill>,
}

impl SkillTool {
    pub fn new(skills: Vec<Skill>) -> Self {
        Self { skills }
    }
}

impl crate::tools::Tool for SkillTool {
    fn name(&self) -> String {
        "skill".to_string()
    }

    fn description(&self) -> String {
        "Load the full instructions of an installed Agent Skill. Available skills are listed in your system prompt.".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Skill name, as listed in the system prompt" }
            },
            "required": ["name"]
        })
    }

    fn approval_key(&self, _args: &serde_json::Value) -> Option<crate::permissions::ApprovalKey> {
        None // read-only
    }

    fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let name = args
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .trim();
        if name.is_empty() {
            bail!("'name' must not be empty");
        }
        let Some(skill) = self.skills.iter().find(|s| s.name == name) else {
            bail!("Unknown skill '{name}'. Available: {}", self.skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", "));
        };
        let content = load(skill)?;
        const CAP: usize = 30_000;
        if content.chars().count() > CAP {
            let mut t: String = content.chars().take(CAP).collect();
            t.push_str("\n[skill content truncated]");
            Ok(t)
        } else {
            Ok(content)
        }
    }
}

pub fn system_prompt_listing(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n\nAvailable Agent Skills (load with the 'skill' tool):\n");
    for s in skills {
        out.push_str(&format!("- {}: {}\n", s.name, s.description));
    }
    out
}

// ── Agent-managed skills ────────────────────────────────────────────────────

/// Caps: skills stay small enough to inject.
pub const SKILL_MD_CAP: usize = 32 * 1024;
pub const SKILL_FILE_CAP: usize = 64 * 1024;

const SKILL_SUBDIRS: &[&str] = &["references", "templates", "scripts", "assets"];

/// Validate a skill name (filesystem-safe, discovery-stable).
fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 64 {
        bail!("Skill name must be 1-64 characters");
    }
    if !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_') {
        bail!("Skill name must be lowercase letters, digits, '-' or '_'");
    }
    Ok(())
}

/// Validate SKILL.md frontmatter: opens with `---`, closes with `---`, has a
/// matching `name:` and a non-empty `description:`, and a non-empty body.
fn validate_frontmatter(content: &str, name: &str) -> Result<()> {
    if content.chars().count() > SKILL_MD_CAP {
        bail!("SKILL.md too large ({} char cap)", SKILL_MD_CAP);
    }
    let mut lines = content.lines();
    if lines.next() != Some("---") {
        bail!("SKILL.md must start with '---' frontmatter");
    }
    let mut found_name = false;
    let mut found_desc = false;
    let mut body_lines = 0usize;
    let mut in_body = false;
    for line in lines {
        if !in_body {
            if line.trim() == "---" {
                in_body = true;
                continue;
            }
            if let Some(v) = line.strip_prefix("name:") {
                if v.trim().trim_matches('"') == name {
                    found_name = true;
                }
            }
            if let Some(v) = line.strip_prefix("description:") {
                if !v.trim().is_empty() {
                    found_desc = true;
                }
            }
        } else if !line.trim().is_empty() {
            body_lines += 1;
        }
    }
    if !in_body {
        bail!("SKILL.md frontmatter never closes with '---'");
    }
    if !found_name {
        bail!("Frontmatter 'name:' must match the skill name '{name}'");
    }
    if !found_desc {
        bail!("Frontmatter needs a non-empty 'description:'");
    }
    if body_lines == 0 {
        bail!("SKILL.md needs a body after the frontmatter");
    }
    Ok(())
}

/// Authoring tool (orchestrator-only; subagents stay consumers).
/// Project scope is team-shared, global scope is personal.
pub struct ManageSkillTool {
    project_root: std::path::PathBuf,
    global_root: Option<std::path::PathBuf>,
}

impl ManageSkillTool {
    pub fn new(project_skills: std::path::PathBuf, global_skills: Option<std::path::PathBuf>) -> Self {
        Self { project_root: project_skills, global_root: global_skills }
    }

    fn root_for(&self, scope: &str) -> Result<std::path::PathBuf> {
        match scope {
            "global" => self.global_root.clone().context("No global skills directory configured"),
            _ => Ok(self.project_root.clone()),
        }
    }

    /// Resolve a skill dir: project first, then global (unless scoped).
    fn resolve(&self, name: &str, scope: Option<&str>) -> Result<(std::path::PathBuf, &'static str)> {
        validate_name(name)?;
        match scope {
            Some("global") => Ok((self.root_for("global")?.join(name), "global")),
            Some(_) | None => {
                let project_dir = self.root_for("project")?.join(name);
                if project_dir.join("SKILL.md").is_file() {
                    return Ok((project_dir, "project"));
                }
                if scope.is_none() {
                    if let Ok(global) = self.root_for("global") {
                        let global_dir = global.join(name);
                        if global_dir.join("SKILL.md").is_file() {
                            return Ok((global_dir, "global"));
                        }
                    }
                }
                Ok((project_dir, "project"))
            }
        }
    }

    /// Resolve a supporting-file path inside a skill dir. Constrained to the
    /// known subdirectories; `..` and absolute paths rejected.
    fn support_file(dir: &std::path::Path, file_path: &str) -> Result<std::path::PathBuf> {
        let rel = std::path::Path::new(file_path);
        if rel.is_absolute() {
            bail!("Supporting path must be relative, e.g. references/api.md");
        }
        let mut comps = rel.components();
        let first = comps.next().ok_or_else(|| anyhow::anyhow!("Empty supporting path"))?;
        let first = first.as_os_str().to_str().unwrap_or("");
        if !SKILL_SUBDIRS.contains(&first) {
            bail!("Supporting files live under references/, templates/, scripts/, or assets/");
        }
        let mut out = dir.to_path_buf();
        for c in std::path::Path::new(file_path).components() {
            match c {
                std::path::Component::Normal(s) => out.push(s),
                _ => bail!("Supporting path must not contain '..', '.', or prefixes"),
            }
        }
        Ok(out)
    }
}

impl crate::tools::Tool for ManageSkillTool {
    fn name(&self) -> String {
        "manage_skill".to_string()
    }

    fn description(&self) -> String {
        "Create and maintain SKILL.md skills (reusable procedures for recurring task types). Create when a complex task succeeded, errors were overcome, or a non-trivial workflow was discovered. Patch immediately when a used skill proved stale or incomplete. Skills live in project .catapult/skills (default scope) or global scope. Every write asks for approval.".to_string()
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["create", "patch", "edit", "delete", "write_file", "remove_file"], "description": "create: new SKILL.md; patch: targeted old/new_string fix (preferred); edit: full rewrite; delete; write_file/remove_file: supporting files" },
                "name": { "type": "string", "description": "Skill name (lowercase, hyphens, matches frontmatter)" },
                "content": { "type": "string", "description": "Full SKILL.md (create/edit), with --- frontmatter" },
                "old_string": { "type": "string", "description": "Text to find (patch; must be unique unless replace_all)" },
                "new_string": { "type": "string", "description": "Replacement text (patch)" },
                "replace_all": { "type": "boolean", "description": "Replace all occurrences (patch)" },
                "file_path": { "type": "string", "description": "Supporting path like references/api.md (write_file/remove_file)" },
                "file_content": { "type": "string", "description": "Content for the file (write_file)" },
                "scope": { "type": "string", "enum": ["project", "global"], "description": "Which skills dir (default: project)" }
            },
            "required": ["action", "name"]
        })
    }

    fn approval_key(&self, _args: &Value) -> Option<crate::permissions::ApprovalKey> {
        // Curating shared knowledge is always a deliberate, approved act.
        Some(crate::permissions::ApprovalKey { tool: "manage_skill".into(), command: None })
    }

    fn execute(&self, args: &Value) -> Result<String> {
        let action = args.get("action").and_then(|a| a.as_str()).unwrap_or("");
        let name = args.get("name").and_then(|n| n.as_str()).unwrap_or("").trim().to_string();
        let scope = args.get("scope").and_then(|s| s.as_str());
        match action {
            "create" => {
                validate_name(&name)?;
                let content = args.get("content").and_then(|c| c.as_str()).unwrap_or("");
                validate_frontmatter(content, &name)?;
                // Same-scope collisions rejected; a project skill may
                // shadow a global one (discovery prefers project).
                let (dir, used) = self.resolve(&name, scope.or(Some("project")))?;
                if dir.join("SKILL.md").is_file() {
                    bail!("A skill named '{name}' already exists in {used} scope");
                }
                std::fs::create_dir_all(&dir)
                    .with_context(|| format!("Cannot create {}", dir.display()))?;
                std::fs::write(dir.join("SKILL.md"), content)
                    .with_context(|| format!("Cannot write {}", dir.display()))?;
                Ok(format!("Skill '{name}' created in {used} scope"))
            }
            "patch" => {
                let (dir, used) = self.resolve(&name, scope)?;
                let file = dir.join("SKILL.md");
                let current = std::fs::read_to_string(&file)
                    .with_context(|| format!("Cannot read {}", file.display()))?;
                let old = args.get("old_string").and_then(|s| s.as_str()).unwrap_or("");
                let new = args.get("new_string").and_then(|s| s.as_str()).unwrap_or("");
                if old.is_empty() {
                    bail!("'old_string' must not be empty for patch");
                }
                let replace_all = args.get("replace_all").and_then(|b| b.as_bool()).unwrap_or(false);
                let count = current.matches(old).count();
                if count == 0 {
                    bail!("'old_string' not found in {name}/SKILL.md");
                }
                if count > 1 && !replace_all {
                    bail!("'old_string' matches {count} places — add context or pass replace_all");
                }
                let updated = if replace_all { current.replace(old, &new) } else { current.replacen(old, &new, 1) };
                validate_frontmatter(&updated, &name)?;
                std::fs::write(&file, &updated)
                    .with_context(|| format!("Cannot write {}", file.display()))?;
                Ok(format!("Skill '{name}' patched in {used} scope"))
            }
            "edit" => {
                let (dir, used) = self.resolve(&name, scope)?;
                let file = dir.join("SKILL.md");
                if !file.is_file() {
                    bail!("Unknown skill '{name}'");
                }
                let content = args.get("content").and_then(|c| c.as_str()).unwrap_or("");
                validate_frontmatter(content, &name)?;
                std::fs::write(&file, content)
                    .with_context(|| format!("Cannot write {}", file.display()))?;
                Ok(format!("Skill '{name}' rewritten in {used} scope"))
            }
            "delete" => {
                let (dir, used) = self.resolve(&name, scope)?;
                if !dir.join("SKILL.md").is_file() {
                    bail!("Unknown skill '{name}'");
                }
                std::fs::remove_dir_all(&dir)
                    .with_context(|| format!("Cannot delete {}", dir.display()))?;
                Ok(format!("Skill '{name}' deleted from {used} scope"))
            }
            "write_file" => {
                let (dir, used) = self.resolve(&name, scope)?;
                if !dir.join("SKILL.md").is_file() {
                    bail!("Unknown skill '{name}'");
                }
                let rel = args.get("file_path").and_then(|f| f.as_str()).unwrap_or("");
                let target = Self::support_file(&dir, rel)?;
                let content = args.get("file_content").and_then(|c| c.as_str()).unwrap_or("");
                if content.chars().count() > SKILL_FILE_CAP {
                    bail!("Supporting file too large ({} char cap)", SKILL_FILE_CAP);
                }
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)
                        .with_context(|| format!("Cannot create directory {}", parent.display()))?;
                }
                std::fs::write(&target, content)
                    .with_context(|| format!("Cannot write {}", target.display()))?;
                Ok(format!("Wrote {rel} in skill '{name}' ({used} scope)"))
            }
            "remove_file" => {
                let (dir, used) = self.resolve(&name, scope)?;
                let rel = args.get("file_path").and_then(|f| f.as_str()).unwrap_or("");
                let target = Self::support_file(&dir, rel)?;
                if !target.is_file() {
                    bail!("'{rel}' not found in skill '{name}'");
                }
                std::fs::remove_file(&target)
                    .with_context(|| format!("Cannot delete {}", target.display()))?;
                Ok(format!("Removed {rel} from skill '{name}' ({used} scope)"))
            }
            _ => bail!("Unknown action '{action}' (expected create, patch, edit, delete, write_file, or remove_file)"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Tool as _;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("harness-skills-{}-{}", label, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn discovers_skills_with_frontmatter() {
        let global = temp_dir("global");
        let skill_dir = global.join("release-notes");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: release-notes\ndescription: Write changelog entries in house style\n---\n\n# Instructions\nWrite notes.\n",
        )
        .unwrap();
        let skills = discover(&[global.clone()]);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "release-notes");
        assert_eq!(skills[0].description, "Write changelog entries in house style");
        assert!(skills[0].path.ends_with("SKILL.md"));
        std::fs::remove_dir_all(&global).unwrap();
    }

    #[test]
    fn project_skills_override_global() {
        let global = temp_dir("proj-global");
        let project = temp_dir("proj-local");
        for (root, desc) in [(&global, "global version"), (&project, "project version")] {
            let dir = root.join("deploy");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("SKILL.md"),
                format!("---\nname: deploy\ndescription: {desc}\n---\n\n# Deploy"),
            )
            .unwrap();
        }
        let skills = discover(&[global, project]);
        assert_eq!(skills.len(), 1, "name conflicts resolve to one entry");
        assert_eq!(skills[0].description, "project version");
    }

    #[test]
    fn missing_skill_file_is_skipped() {
        let root = temp_dir("skip");
        std::fs::create_dir_all(root.join("broken")).unwrap();
        assert!(discover(&[root.clone()]).is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn skill_tool_loads_content_and_fails_loud() {
        let root = temp_dir("tool");
        let dir = root.join("deploy");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), "---\nname: deploy\ndescription: d\n---\nSTEPS HERE").unwrap();
        let tool = SkillTool { skills: discover(&[root.clone()]) };
        assert_eq!(tool.name(), "skill");
        let out = tool.execute(&serde_json::json!({"name": "deploy"})).unwrap();
        assert!(out.contains("STEPS HERE"));
        assert!(tool.execute(&serde_json::json!({"name": "missing"})).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    fn manage_tool(label: &str) -> (ManageSkillTool, PathBuf, PathBuf) {
        let base = temp_dir(label);
        let project = base.join("proj");
        let global = base.join("glob");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&global).unwrap();
        (ManageSkillTool::new(project.clone(), Some(global.clone())), project, base)
    }

    fn skill_md(name: &str) -> String {
        format!("---\nname: {name}\ndescription: Does things.\n---\n\n# Steps\n1. Do it.")
    }

    #[test]
    fn manage_create_validates_and_discovers() {
        let (tool, project, base) = manage_tool("manage-create");
        assert_eq!(tool.name(), "manage_skill");
        assert!(tool.approval_key(&serde_json::json!({"action": "create"})).is_some());
        assert!(tool.execute(&serde_json::json!({"action": "create", "name": "Bad Name!", "content": skill_md("x")})).is_err());
        assert!(tool.execute(&serde_json::json!({"action": "create", "name": "s", "content": "no frontmatter"})).is_err());
        assert!(tool.execute(&serde_json::json!({"action": "create", "name": "s", "content": "---\nname: s\n---\nbody"})).is_err());
        assert!(tool.execute(&serde_json::json!({"action": "create", "name": "s", "content": "---\nname: other\ndescription: d\n---\nbody"})).is_err());
        assert!(tool.execute(&serde_json::json!({"action": "create", "name": "s", "content": "---\nname: s\ndescription: d\n---\n"})).is_err());
        let out = tool.execute(&serde_json::json!({"action": "create", "name": "deploy", "content": skill_md("deploy")})).unwrap();
        assert!(out.contains("project"));
        let found = discover(&[project]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].description, "Does things.");
        assert!(tool.execute(&serde_json::json!({"action": "create", "name": "deploy", "content": skill_md("deploy")})).is_err());
        tool.execute(&serde_json::json!({"action": "create", "name": "deploy", "content": skill_md("deploy"), "scope": "global"})).unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn manage_patch_edit_delete_roundtrip() {
        let (tool, _project, base) = manage_tool("manage-roundtrip");
        tool.execute(&serde_json::json!({"action": "create", "name": "fixer", "content": skill_md("fixer")})).unwrap();
        let out = tool.execute(&serde_json::json!({"action": "patch", "name": "fixer", "old_string": "Do it.", "new_string": "Do it well."})).unwrap();
        assert!(out.contains("patched"));
        assert!(tool.execute(&serde_json::json!({"action": "patch", "name": "fixer", "old_string": "missing", "new_string": "x"})).is_err());
        tool.execute(&serde_json::json!({"action": "create", "name": "other", "content": skill_md("other")})).unwrap();
        assert!(tool.execute(&serde_json::json!({"action": "edit", "name": "fixer", "content": "nope"})).is_err());
        tool.execute(&serde_json::json!({"action": "edit", "name": "fixer", "content": skill_md("fixer")})).unwrap();
        assert!(tool.execute(&serde_json::json!({"action": "write_file", "name": "fixer", "file_path": "../evil.md", "file_content": "x"})).is_err());
        assert!(tool.execute(&serde_json::json!({"action": "write_file", "name": "fixer", "file_path": "data/x.md", "file_content": "x"})).is_err());
        tool.execute(&serde_json::json!({"action": "write_file", "name": "fixer", "file_path": "references/api.md", "file_content": "API"})).unwrap();
        tool.execute(&serde_json::json!({"action": "remove_file", "name": "fixer", "file_path": "references/api.md"})).unwrap();
        assert!(tool.execute(&serde_json::json!({"action": "remove_file", "name": "fixer", "file_path": "references/api.md"})).is_err());
        tool.execute(&serde_json::json!({"action": "delete", "name": "fixer"})).unwrap();
        assert!(tool.execute(&serde_json::json!({"action": "delete", "name": "fixer"})).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn manage_scopes_stay_separate() {
        let (tool, _project, base) = manage_tool("manage-scopes");
        tool.execute(&serde_json::json!({"action": "create", "name": "mine", "content": skill_md("mine"), "scope": "global"})).unwrap();
        let out = tool.execute(&serde_json::json!({"action": "patch", "name": "mine", "old_string": "Do it.", "new_string": "Done well."})).unwrap();
        assert!(out.contains("global"));
        tool.execute(&serde_json::json!({"action": "create", "name": "mine", "content": skill_md("mine")})).unwrap();
        let out = tool.execute(&serde_json::json!({"action": "patch", "name": "mine", "old_string": "Do it.", "new_string": "Done better."})).unwrap();
        assert!(out.contains("project"));
        let _ = std::fs::remove_dir_all(&base);
    }
}
