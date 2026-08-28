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

// ── Windows `.cmd`/`.bat` shim wrapping ──────────────────────────────────────
//
// llama.cpp spawns MCP servers with CreateProcess (sheredom subprocess.h),
// which cannot launch `.cmd`/`.bat` scripts — package managers like `npx` (and
// `uvx`, `pipx`, `yarn`, ...) ship as `.cmd` shims on Windows, so a bare
// `command: "npx"` fails with "failed to spawn". On Windows we wrap those
// commands as `cmd /c <command> <args>` in an *effective* runtime config; the
// persisted `mcp.json` keeps the portable form.

const PATHEXT_DEFAULT: [&str; 4] = [".COM", ".EXE", ".BAT", ".CMD"];

fn ext_means_script(ext: &str) -> bool {
    matches!(ext.to_ascii_lowercase().as_str(), ".cmd" | ".bat")
}

/// True when Windows `CreateProcess` could not launch `command` directly and
/// it must be run through `cmd.exe /c`. Mirrors `SearchPathW`: a bare name is
/// resolved against `current_dir` then the `dirs` (PATH) directories, trying
/// each PATHEXT extension in order; paths with a separator are probed as-is.
///
/// Resolution gaps are conservative: a bare name that resolves to a `.cmd`/
/// `.bat` is wrapped, and one that resolves to *nothing* on PATH is wrapped too
/// (an unresolvable bare command is most likely a shim the calling process
/// cannot see, and `cmd /c` is no worse for a genuinely missing binary).
pub fn needs_cmd_wrapper(
    command: &str,
    exts: &[String],
    dirs: &[PathBuf],
    current_dir: Option<&Path>,
) -> bool {
    if command.trim().is_empty() {
        return false;
    }
    let lower = command.to_ascii_lowercase();
    if lower.ends_with(".cmd") || lower.ends_with(".bat") {
        return true;
    }
    if lower.ends_with(".exe") || lower.ends_with(".com") {
        return false;
    }
    if command.contains('/') || command.contains('\\') {
        let probe = Path::new(command);
        if probe.extension().is_none() {
            for ext in exts {
                let mut os = probe.as_os_str().to_os_string();
                os.push(ext);
                if Path::new(&os).is_file() {
                    return ext_means_script(ext);
                }
            }
        }
        return false;
    }
    let mut search: Vec<&Path> = current_dir.into_iter().collect();
    search.extend(dirs.iter().map(|d| d.as_path()));
    for dir in search {
        let base = dir.join(command);
        if base.is_file() {
            return false;
        }
        for ext in exts {
            let mut os = base.as_os_str().to_os_string();
            os.push(ext);
            if Path::new(&os).is_file() {
                return ext_means_script(ext);
            }
        }
    }
    // Bare name, no path separator, not found anywhere: wrap defensively.
    true
}

/// Rewrite a server that needs the wrapper into `cmd /c <command> <args>`.
fn wrap_server(s: &McpServer) -> McpServer {
    let mut args = Vec::with_capacity(s.args.len() + 2);
    args.push("/c".to_string());
    args.push(s.command.clone());
    args.extend(s.args.iter().cloned());
    McpServer {
        command: "cmd".to_string(),
        args,
        env: s.env.clone(),
        cwd: s.cwd.clone(),
        timeout_ms: s.timeout_ms,
    }
}

/// Apply the shim wrapping to a config. Pure over the injected environment so
/// it is unit-testable on any platform.
fn apply_shim_wrap(
    cfg: &McpConfig,
    exts: &[String],
    dirs: &[PathBuf],
    current_dir: Option<&Path>,
) -> McpConfig {
    let mut out = cfg.clone();
    for (name, s) in cfg.servers.iter() {
        if needs_cmd_wrapper(&s.command, exts, dirs, current_dir) {
            out.servers.insert(name.clone(), wrap_server(s));
        }
    }
    out
}

/// The config actually handed to llama.cpp: on Windows, `.cmd`/`.bat` commands
/// are rewrapped through `cmd /c`; elsewhere it is the persisted config as-is.
pub fn effective_config(cfg: &McpConfig) -> McpConfig {
    #[cfg(target_os = "windows")]
    {
        let exts: Vec<String> = match std::env::var("PATHEXT") {
            Ok(v) => v.split(';').filter(|s| !s.is_empty()).map(str::to_string).collect(),
            Err(_) => PATHEXT_DEFAULT.iter().map(|s| s.to_string()).collect(),
        };
        let dirs: Vec<PathBuf> = match std::env::var("PATH") {
            Ok(v) => v.split(';').filter(|s| !s.is_empty()).map(PathBuf::from).collect(),
            Err(_) => Vec::new(),
        };
        apply_shim_wrap(cfg, &exts, &dirs, std::env::current_dir().ok().as_deref())
    }
    #[cfg(not(target_os = "windows"))]
    {
        cfg.clone()
    }
}

fn effective_path() -> Result<PathBuf> {
    Ok(mcp_config_path()?.with_file_name("mcp_effective.json"))
}

fn remove_stale_effective() {
    if let Ok(p) = effective_path() {
        let _ = std::fs::remove_file(p);
    }
}

/// Path to pass as `--mcp-servers-config`. When the effective config differs
/// from the persisted one (Windows shim wrap applied), a runtime copy is
/// materialized at `mcp_effective.json` so `mcp.json` stays portable; the copy
/// is removed again once unused so llama.cpp never reads a stale file.
pub fn runtime_mcp_config_path() -> Result<Option<PathBuf>> {
    let cfg = load()?;
    if cfg.servers.is_empty() {
        remove_stale_effective();
        return Ok(None);
    }
    let effective = effective_config(&cfg);
    if serde_json::to_string(&effective)? == serde_json::to_string(&cfg)? {
        remove_stale_effective();
        return Ok(Some(mcp_config_path()?));
    }
    let path = effective_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(&effective)?)?;
    Ok(Some(path))
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

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "catapult-mcp-test-{}-{}",
            std::process::id(),
            label
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn exts(entries: &[&str]) -> Vec<String> {
        entries.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn shim_needs_wrapper_by_extension() {
        let dir = temp_dir("shim-ext");
        assert!(needs_cmd_wrapper("C:/tools/redis.cmd", &exts(&[".CMD"]), &[], Some(&dir)));
        assert!(needs_cmd_wrapper("C:/tools/redis.bat", &exts(&[".BAT"]), &[], Some(&dir)));
        assert!(needs_cmd_wrapper("C:/tools/redis.CMD", &exts(&[".CMD"]), &[], Some(&dir)));
        assert!(!needs_cmd_wrapper("C:/tools/redis.exe", &exts(&[".EXE"]), &[], Some(&dir)));
        assert!(!needs_cmd_wrapper("C:/tools/redis.com", &exts(&[".COM"]), &[], Some(&dir)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bare_shim_resolves_through_path_order() {
        let dir = temp_dir("shim-path");
        std::fs::write(dir.join("npx.cmd"), "").unwrap();
        // Bare name + .CMD shim found in the PATH dir -> wrap.
        assert!(needs_cmd_wrapper("npx", &exts(&[".CMD"]), &[dir.clone()], None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn real_exe_wins_over_shim() {
        let dir = temp_dir("shim-exe");
        std::fs::write(dir.join("npx.exe"), "").unwrap();
        std::fs::write(dir.join("npx.cmd"), "").unwrap();
        // .EXE is tried first in PATHEXT order -> no wrap.
        assert!(!needs_cmd_wrapper(
            "npx",
            &exts(&[".EXE", ".CMD"]),
            &[dir.clone()],
            None
        ));
        // Reversed order resolves the .CMD first -> wrap.
        assert!(needs_cmd_wrapper("npx", &exts(&[".CMD", ".EXE"]), &[dir.clone()], None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn path_with_separator_probes_extensions() {
        let dir = temp_dir("shim-rel");
        std::fs::create_dir_all(dir.join("tools")).unwrap();
        std::fs::write(dir.join("tools").join("serve.cmd"), "").unwrap();
        let target = dir.join("tools").join("serve");
        let probe = target.to_string_lossy();
        assert!(needs_cmd_wrapper(&probe, &exts(&[".EXE", ".CMD"]), &[], Some(&dir)));
        assert!(!needs_cmd_wrapper(&probe, &exts(&[".EXE"]), &[], Some(&dir)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unresolved_bare_name_is_wrapped_defensively() {
        let dir = temp_dir("shim-unknown");
        // Bare name, not found anywhere: conservatively wrap — PATH gaps must
        // never defeat the fix.
        assert!(needs_cmd_wrapper("gobbledygook", &exts(&[".EXE", ".CMD"]), &[dir.clone()], None));
        // Empty commands are skipped entirely.
        assert!(!needs_cmd_wrapper("", &exts(&[".EXE"]), &[dir.clone()], None));
        // Explicit exe from a real dir is never wrapped.
        std::fs::write(dir.join("real.exe"), "").unwrap();
        assert!(!needs_cmd_wrapper("real", &exts(&[".EXE"]), &[dir.clone()], None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shim_wrap_rewrites_only_scripts() {
        let dir = temp_dir("wrap");
        std::fs::write(dir.join("npx.cmd"), "").unwrap();

        let mut servers = BTreeMap::new();
        let npx = McpServer {
            command: "npx".to_string(),
            args: vec!["-y".to_string(), "exa-mcp-server".to_string()],
            env: HashMap::new(),
            cwd: None,
            timeout_ms: Some(15000),
        };
        let exe = McpServer {
            command: "C:/tools/exa.exe".to_string(),
            args: vec![],
            env: HashMap::new(),
            cwd: None,
            timeout_ms: None,
        };
        servers.insert("exa".to_string(), npx);
        servers.insert("exe".to_string(), exe);
        let cfg = McpConfig {
            servers: servers.clone(),
        };

        let wrapped = apply_shim_wrap(&cfg, &exts(&[".CMD"]), &[dir.clone()], None);

        let exa = wrapped.servers.get("exa").unwrap();
        assert_eq!(exa.command, "cmd");
        assert_eq!(exa.args, vec!["/c", "npx", "-y", "exa-mcp-server"]);
        assert_eq!(exa.timeout_ms, Some(15000), "env/cwd/timeout survive the wrap");
        let exe_entry = wrapped.servers.get("exe").unwrap();
        assert_eq!(exe_entry.command, "C:/tools/exa.exe");
        assert!(exe_entry.args.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn windows_host_effective_config_wraps_npx_shim() {
        // Environment-driven: only meaningful on Windows hosts where `npx`
        // resolves to a `.cmd` shim (a normal Node install). Skips cleanly
        // when the precondition does not hold (e.g. CI without Node).
        #[cfg(target_os = "windows")]
        {
            let exts: Vec<String> = match std::env::var("PATHEXT") {
                Ok(v) => v.split(';').filter(|s| !s.is_empty()).map(str::to_string).collect(),
                Err(_) => PATHEXT_DEFAULT.iter().map(|s| s.to_string()).collect(),
            };
            let dirs: Vec<PathBuf> = match std::env::var("PATH") {
                Ok(v) => v.split(';').filter(|s| !s.is_empty()).map(PathBuf::from).collect(),
                Err(_) => Vec::new(),
            };
            if !needs_cmd_wrapper("npx", &exts, &dirs, None) {
                return;
            }
            let mut servers = BTreeMap::new();
            servers.insert(
                "exa".to_string(),
                McpServer {
                    command: "npx".to_string(),
                    args: vec!["-y".to_string(), "exa-mcp-server".to_string()],
                    env: HashMap::new(),
                    cwd: None,
                    timeout_ms: None,
                },
            );
            let effective = effective_config(&McpConfig { servers });
            let exa = effective.servers.get("exa").unwrap();
            assert_eq!(exa.command, "cmd");
            assert_eq!(exa.args, vec!["/c", "npx", "-y", "exa-mcp-server"]);
        }
        #[cfg(not(target_os = "windows"))]
        {
            // POSIX has no `.cmd` shim regime; nothing to assert.
        }
    }
}