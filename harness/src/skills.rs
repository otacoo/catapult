//! Agent Skills discovery (agentskills.io-style `SKILL.md` folders).
//!
//! Skills are folders containing a `SKILL.md` with optional YAML frontmatter:
//!
//! ```text
//! skills/
//!   pdf-forms/SKILL.md       ← global ({data_dir}/catapult/skills/)
//! project/.catapult/skills/deploy/SKILL.md
//! ```
//!
//! Discovery is progressive disclosure: only names + descriptions are shown
//! to the model (appended to the system prompt); the `skill` tool loads the
//! full instructions on demand.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};

#[derive(Debug, Clone)]
pub struct Skill {
    /// Name from frontmatter (falls back to the folder name).
    pub name: String,
    /// Short description from frontmatter (shown in the system prompt).
    pub description: String,
    /// Path to the SKILL.md file.
    pub path: PathBuf,
}

/// Parse `name`/`description` out of a `SKILL.md` frontmatter block
/// (`---` … `---`), tolerating missing fields.
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
            // Project-local entries win over global ones with the same name.
            out.retain(|s: &Skill| s.name != name);
            out.push(Skill { name, description, path: skill_file });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Full instructions for a skill (the `skill` tool returns this).
pub fn load(skill: &Skill) -> Result<String> {
    std::fs::read_to_string(&skill.path)
        .with_context(|| format!("Cannot read skill {}", skill.path.display()))
}

/// A tool that expands a discovered skill's instructions on demand.
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

/// Compact listing injected into the system prompt (name — description).
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
}
