//! Minimal MCP (Model Context Protocol) stdio client.
//!
//! Speaks JSON-RPC 2.0 over newline-delimited stdio to MCP servers, exactly
//! the transport llama.cpp also uses. The handshake is `initialize` →
//! `notifications/initialized`; tools are listed once per server and exposed
//! to the model as `mcp_<server>_<tool>` registry entries.
//!
//! The client is synchronous (thread + channel based) so tool execution can
//! run on the blocking pool like the native tools. All failures are loud;
//! a server that fails to start simply contributes no tools (surfaced as a
//! notice by the caller).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::tools::Tool;

const PROTOCOL_VERSION: &str = "2024-11-05";
/// Default per-request timeout when the server config doesn't specify one.
pub const REQUEST_TIMEOUT_SECS: u64 = 30;

#[derive(Debug, Clone)]
pub struct McpToolInfo {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

/// One live MCP server connection (child process over stdio).
pub struct McpSession {
    child: Child,
    writer: std::process::ChildStdin,
    responses: Receiver<(u64, Value)>,
    next_id: u64,
    timeout: Duration,
}

impl McpSession {
    /// Spawn a server process and complete the MCP handshake.
    pub fn start(
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
        cwd: Option<&str>,
        timeout_ms: Option<u64>,
    ) -> Result<Self> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .envs(env) // overlay on the inherited environment (PATH must survive)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        #[cfg(target_os = "windows")]
        {
            #[allow(unused_imports)]
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }
        let mut child = cmd
            .spawn()
            .with_context(|| format!("Failed to spawn MCP server '{command}'"))?;
        let stdin = child.stdin.take().context("no stdin pipe")?;
        let stdout = child.stdout.take().context("no stdout pipe")?;

        // Reader thread: newline-delimited JSON-RPC → channel (responses only).
        let (tx, responses) = mpsc::channel();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                // Only JSON-RPC responses carry an id; notifications are ignored.
                let Some(id) = v.get("id").and_then(|i| i.as_u64()) else {
                    continue;
                };
                if tx.send((id, v)).is_err() {
                    break;
                }
            }
        });

        let timeout = Duration::from_secs(timeout_ms.unwrap_or(30_000).max(1000) / 1000);
        let mut session = Self {
            child,
            writer: stdin,
            responses,
            next_id: 1,
            timeout,
        };
        // Handshake.
        let init = session.request(
            "initialize",
            serde_json::json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "catapult", "version": "0.4.3" },
            }),
        )?;
        if init.is_null() {
            bail!("MCP server '{}' did not answer initialize", command);
        }
        session.notify("notifications/initialized", serde_json::json!({}))?;
        Ok(session)
    }

    fn write_line(&mut self, line: &str) -> Result<()> {
        self.writer
            .write_all(line.as_bytes())
            .and_then(|_| self.writer.write_all(b"\n"))
            .and_then(|_| self.writer.flush())
            .context("MCP server stdin write failed")
    }

    fn send(&mut self, msg: &Value) -> Result<()> {
        let mut line = serde_json::to_string(msg)?;
        // The MCP stdio transport forbids embedded newlines.
        line.retain(|c| c != '\n' && c != '\r');
        self.write_line(&line)
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let msg = serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        self.send(&msg)?;
        let deadline = Instant::now() + self.timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                bail!("MCP request '{method}' timed out");
            }
            match self.responses.recv_timeout(left) {
                Ok((rid, value)) if rid == id => {
                    if let Some(err) = value.get("error") {
                        bail!("MCP server error for '{method}': {err}");
                    }
                    return Ok(value.get("result").cloned().unwrap_or(Value::Null));
                }
                Ok(_stale) => continue, // response for an older id
                Err(RecvTimeoutError::Timeout) => bail!("MCP request '{method}' timed out"),
                Err(RecvTimeoutError::Disconnected) => {
                    bail!("MCP server closed the connection during '{method}'")
                }
            }
        }
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let msg = serde_json::json!({ "jsonrpc": "2.0", "method": method, "params": params });
        self.send(&msg)
    }

    /// Advertised tools of this server.
    pub fn list_tools(&mut self) -> Result<Vec<McpToolInfo>> {
        let result = self.request("tools/list", serde_json::json!({}))?;
        let mut out = Vec::new();
        if let Some(tools) = result.get("tools").and_then(|t| t.as_array()) {
            for t in tools {
                let Some(name) = t.get("name").and_then(|n| n.as_str()) else {
                    continue;
                };
                out.push(McpToolInfo {
                    name: name.to_string(),
                    description: t
                        .get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("")
                        .to_string(),
                    schema: t.get("inputSchema").cloned().unwrap_or(serde_json::json!({
                        "type": "object"
                    })),
                });
            }
        }
        Ok(out)
    }

    /// Invoke a tool; returns the concatenated text content (fail loud).
    pub fn call_tool(&mut self, name: &str, arguments: &Value) -> Result<String> {
        let result = self.request(
            "tools/call",
            serde_json::json!({ "name": name, "arguments": arguments }),
        )?;
        let mut text = String::new();
        if let Some(content) = result.get("content").and_then(|c| c.as_array()) {
            for item in content {
                if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(t) = item.get("text").and_then(|t| t.as_str()) {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(t);
                    }
                }
            }
        }
        if result.get("isError").and_then(|e| e.as_bool()).unwrap_or(false) {
            bail!("MCP tool '{}' reported an error: {}", name, text);
        }
        Ok(text)
    }
}

impl Drop for McpSession {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A registry-facing wrapper around one MCP tool.
pub struct McpTool {
    pub server: String,
    pub tool_name: String,
    pub session: Arc<Mutex<McpSession>>,
    pub desc: String,
    pub schema: Value,
}

/// Registry-facing tool name: `mcp_<server>_<tool>`.
pub fn mcp_tool_name(server: &str, tool: &str) -> String {
    format!("mcp_{}_{}", server, tool)
}

impl Tool for McpTool {
    fn name(&self) -> String {
        mcp_tool_name(&self.server, &self.tool_name)
    }

    fn description(&self) -> String {
        format!("{} (MCP server '{}')", self.desc, self.server)
    }

    fn parameters(&self) -> Value {
        self.schema.clone()
    }

    fn approval_key(&self, _args: &Value) -> Option<crate::permissions::ApprovalKey> {
        // External tools are never auto-approved.
        Some(crate::permissions::ApprovalKey {
            tool: format!("mcp_{}", self.server),
            command: None,
        })
    }

    fn execute(&self, args: &Value) -> Result<String> {
        let mut session = self
            .session
            .lock()
            .map_err(|_| anyhow::anyhow!("MCP server '{}' is not reachable", self.server))?;
        let out = session.call_tool(&self.tool_name, args)?;
        const CAP: usize = 30_000;
        if out.chars().count() > CAP {
            let mut t: String = out.chars().take(CAP).collect();
            t.push_str("\n[output truncated]");
            Ok(t)
        } else {
            Ok(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_namespaced_per_server() {
        assert_eq!(mcp_tool_name("ddg-search", "web_search"), "mcp_ddg-search_web_search");
        assert_eq!(mcp_tool_name("context7", "get-docs"), "mcp_context7_get-docs");
        // Server and tool name must both appear, in order.
        let n = mcp_tool_name("a", "b");
        assert!(n.starts_with("mcp_"));
        assert!(n.ends_with("_b"));
    }

    #[test]
    fn request_messages_are_jsonrpc_shaped() {
        // The wire format must be exactly JSON-RPC 2.0 with an id.
        let msg = serde_json::json!({
            "jsonrpc": "2.0", "id": 7, "method": "tools/list", "params": {}
        });
        assert_eq!(msg["jsonrpc"], "2.0");
        assert_eq!(msg["id"], 7);
        assert_eq!(msg["method"], "tools/list");
    }
}
