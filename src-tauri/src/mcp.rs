use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::path::PathBuf;

// ── MCP server configuration ────────────────────────────────────────────────
//
// llama.cpp reads a Cursor-compatible JSON file (`--mcp-servers-config`) that
// maps a server name to `{ command, args, env, cwd, timeout_ms }`. We persist
// that exact shape to `{data_dir}/catapult/mcp.json` so it passes through to
// the server unchanged.

/// A single MCP server in the on-disk Cursor-compatible shape.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct McpServer {
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

/// Frontend-facing entry: the server name plus its config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerEntry {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct McpConfig {
    #[serde(default, rename = "mcpServers")]
    pub servers: BTreeMap<String, McpServer>,
}

/// Everything the Tools page needs: the servers and the file path shown to the
/// user (useful for hand-editing).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpInfo {
    pub path: String,
    pub servers: Vec<McpServerEntry>,
}

pub fn mcp_config_path() -> Result<PathBuf> {
    let data_dir = dirs::data_dir()
        .ok_or_else(|| anyhow::anyhow!("Cannot find data directory"))?;
    Ok(data_dir.join("catapult").join("mcp.json"))
}

pub fn load() -> Result<McpConfig> {
    load_at(&mcp_config_path()?)
}

pub fn load_at(path: &Path) -> Result<McpConfig> {
    if !path.exists() {
        return Ok(McpConfig::default());
    }
    let content = std::fs::read_to_string(path)?;
    serde_json::from_str(&content)
        .map_err(|e| anyhow::anyhow!("Invalid MCP config file {}: {}", path.display(), e))
}

pub fn save(entries: &[McpServerEntry]) -> Result<()> {
    save_at(entries, &mcp_config_path()?)
}

pub fn save_at(entries: &[McpServerEntry], path: &Path) -> Result<()> {
    let mut servers = BTreeMap::new();
    for e in entries {
        let name = e.name.trim();
        let command = e.command.trim();
        // Entries without a command are skipped by llama.cpp — don't persist them.
        if name.is_empty() || command.is_empty() {
            continue;
        }
        servers.insert(
            name.to_string(),
            McpServer {
                command: command.to_string(),
                args: e.args.iter().filter(|a| !a.is_empty()).cloned().collect(),
                env: e.env.clone(),
                cwd: e
                    .cwd
                    .as_deref()
                    .map(str::trim)
                    .filter(|c| !c.is_empty())
                    .map(str::to_string),
                timeout_ms: e.timeout_ms,
            },
        );
    }
    let cfg = McpConfig { servers };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(&cfg)?;
    std::fs::write(path, content)?;
    Ok(())
}

pub fn entries_from_config(cfg: &McpConfig) -> Vec<McpServerEntry> {
    cfg.servers
        .iter()
        .map(|(name, s)| McpServerEntry {
            name: name.clone(),
            command: s.command.clone(),
            args: s.args.clone(),
            env: s.env.clone(),
            cwd: s.cwd.clone(),
            timeout_ms: s.timeout_ms,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "catapult-mcp-test-{}-{}.json",
            std::process::id(),
            label
        ))
    }

    fn sample_entries() -> Vec<McpServerEntry> {
        vec![
            McpServerEntry {
                name: "fetch".to_string(),
                command: "npx".to_string(),
                args: vec!["-y".to_string(), "@modelcontextprotocol/server-fetch".to_string()],
                env: HashMap::new(),
                cwd: None,
                timeout_ms: Some(15000),
            },
            McpServerEntry {
                name: "search".to_string(),
                command: "C:/tools/search.exe".to_string(),
                args: vec![],
                env: {
                    let mut m = HashMap::new();
                    m.insert("API_KEY".to_string(), "secret".to_string());
                    m
                },
                cwd: Some("D:/work".to_string()),
                timeout_ms: None,
            },
        ]
    }

    #[test]
    fn save_round_trips_entries() {
        let path = temp_path("roundtrip");
        let _ = std::fs::remove_file(&path);

        let entries = sample_entries();
        save_at(&entries, &path).unwrap();

        let cfg = load_at(&path).unwrap();
        assert_eq!(cfg.servers.len(), 2);
        let restored = entries_from_config(&cfg);
        assert_eq!(restored[0].name, "fetch");
        assert_eq!(restored[0].command, "npx");
        assert_eq!(restored[0].args, vec!["-y", "@modelcontextprotocol/server-fetch"]);
        assert_eq!(restored[0].timeout_ms, Some(15000));
        assert_eq!(restored[1].name, "search");
        assert_eq!(restored[1].env.get("API_KEY").map(|s| s.as_str()), Some("secret"));
        assert_eq!(restored[1].cwd.as_deref(), Some("D:/work"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn file_uses_cursor_compatible_shape() {
        let path = temp_path("shape");
        let _ = std::fs::remove_file(&path);

        save_at(&sample_entries(), &path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        // Key is the Cursor-style "mcpServers" map, not a wrapper array.
        assert!(parsed.get("mcpServers").is_some());
        assert!(parsed.get("mcpServers").unwrap().is_object());
        assert!(parsed["mcpServers"]["fetch"].get("command").is_some());
        assert!(parsed["mcpServers"]["search"]["env"]["API_KEY"].is_string());
        // Empty collections are omitted to keep the file close to hand-written.
        assert!(parsed["mcpServers"]["fetch"].get("env").is_none());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_file_loads_empty() {
        let path = temp_path("missing");
        let _ = std::fs::remove_file(&path);
        let cfg = load_at(&path).unwrap();
        assert!(cfg.servers.is_empty());
    }

    #[test]
    fn entries_without_command_are_dropped() {
        let path = temp_path("emptycmd");
        let _ = std::fs::remove_file(&path);

        save_at(&[McpServerEntry {
            name: "broken".to_string(),
            command: "   ".to_string(),
            args: vec![],
            env: HashMap::new(),
            cwd: None,
            timeout_ms: None,
        }], &path).unwrap();

        let cfg = load_at(&path).unwrap();
        assert!(cfg.servers.is_empty());

        let _ = std::fs::remove_file(&path);
    }
}