//! OpenAI-compatible chat client with SSE streaming.
//!
//! Targets llama.cpp's `/v1/chat/completions` (and any other
//! OpenAI-compatible server). Parses `choices[0].delta` content and
//! tool-call deltas incrementally, and exposes a `StreamCollector` that
//! assembles complete tool calls — with JSON repair via `jsonfix` — so the
//! orchestrator can consume them (Phase 1).

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── Request/response types (OpenAI chat completions shape) ──────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FunctionCall {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    /// OpenAI requires an id; llama.cpp supplies one (e.g. call_xyz).
    #[serde(default)]
    pub id: String,
    #[serde(rename = "type", default = "default_call_type")]
    pub call_type: String,
    pub function: FunctionCall,
}

fn default_call_type() -> String {
    "function".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatMessage {
    /// "system" | "user" | "assistant" | "tool"
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self { role: "user".into(), content: Some(content.into()), tool_calls: None, tool_call_id: None }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self { role: "assistant".into(), content: Some(content.into()), tool_calls: None, tool_call_id: None }
    }
    pub fn system(content: impl Into<String>) -> Self {
        Self { role: "system".into(), content: Some(content.into()), tool_calls: None, tool_call_id: None }
    }
}

// ── Streaming events ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    /// Incremental assistant text.
    Content { text: String },
    /// Incremental tool-call data, assembled by `StreamCollector`.
    ToolCallDelta {
        index: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default)]
        arguments_delta: String,
    },
    Finish { reason: String },
}

/// Accumulates deltas into complete tool calls; argument fragments are repaired
/// via `jsonfix` before parsing in the orchestrator (Phase 1).
#[derive(Debug, Default)]
pub struct StreamCollector {
    calls: Vec<PartialToolCall>,
}

#[derive(Debug, Default)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
}

impl StreamCollector {
    pub fn push(&mut self, ev: &StreamEvent) {
        if let StreamEvent::ToolCallDelta { index, id, name, arguments_delta } = ev {
            while self.calls.len() <= *index {
                self.calls.push(PartialToolCall::default());
            }
            let call = &mut self.calls[*index];
            if let Some(id) = id {
                call.id.push_str(id);
            }
            if let Some(name) = name {
                call.name.push_str(name);
            }
            call.arguments.push_str(arguments_delta);
        }
    }

    /// Complete tool calls with repaired, validated argument JSON. Repair that
    /// still fails to parse is surfaced as an error (fail loud).
    pub fn finish(&self) -> Result<Vec<ToolCall>> {
        self.calls
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let arguments = if c.arguments.trim().is_empty() {
                    "{}".to_string()
                } else {
                    let repaired = crate::jsonfix::repair_json(&c.arguments);
                    if serde_json::from_str::<Value>(&repaired).is_err() {
                        bail!("Tool call {} ({}) has unparseable arguments: {:?}", i, c.name, c.arguments);
                    }
                    repaired
                };
                Ok(ToolCall {
                    id: c.id.clone(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: c.name.clone(),
                        arguments,
                    },
                })
            })
            .collect()
    }
}

// ── Client ──────────────────────────────────────────────────────────────────

pub struct LlmClient {
    base_url: String,
    api_key: Option<String>,
    http: reqwest::Client,
}

/// Parse one SSE `data:` line into stream events. `[DONE]` produces
/// `Finish { reason: "stop" }`. Testable without a server.
pub fn parse_sse_line(line: &str) -> Option<StreamEvent> {
    let data = line.strip_prefix("data:")?.trim();
    if data == "[DONE]" {
        return Some(StreamEvent::Finish { reason: "stop".into() });
    }
    let v: Value = serde_json::from_str(data).ok()?;
    let choice = v.get("choices")?.get(0)?;
    let delta = choice.get("delta")?;
    if let Some(text) = delta.get("content").and_then(|c| c.as_str()) {
        if !text.is_empty() {
            return Some(StreamEvent::Content { text: text.to_string() });
        }
    }
    if let Some(calls) = delta.get("tool_calls").and_then(|c| c.as_array()) {
        // Multiple parallel tool calls in one delta are rare; emit the first
        // with data and the rest with empty deltas is not needed — take each.
        for call in calls {
            let index = call.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
            let id = call.get("id").and_then(|i| i.as_str()).map(String::from);
            let name = call
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
                .map(String::from);
            let args = call
                .get("function")
                .and_then(|f| f.get("arguments"))
                .and_then(|a| a.as_str())
                .unwrap_or("")
                .to_string();
            return Some(StreamEvent::ToolCallDelta {
                index,
                id,
                name,
                arguments_delta: args.to_string(),
            });
        }
    }
    if let Some(reason) = choice.get("finish_reason").and_then(|r| r.as_str()) {
        if !reason.is_empty() {
            return Some(StreamEvent::Finish { reason: reason.to_string() });
        }
    }
    None
}

impl LlmClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self::with_key(base_url, None)
    }

    pub fn with_key(base_url: impl Into<String>, api_key: Option<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key,
            // No overall request timeout: generation streams can run for minutes.
            http: reqwest::Client::builder()
                .build()
                .expect("failed to build HTTP client"),
        }
    }

    /// Stream a chat completion, invoking `on_event` per delta. Returns the
    /// finish reason. `should_stop` is polled between chunks for cooperative
    /// abort (the caller owns the flag).
    pub async fn chat_stream(
        &self,
        model: Option<&str>,
        messages: &[ChatMessage],
        should_stop: impl Fn() -> bool,
        mut on_event: impl FnMut(StreamEvent),
    ) -> Result<String> {
        let mut body = serde_json::json!({
            "messages": messages,
            "stream": true,
        });
        if let Some(m) = model {
            body["model"] = Value::from(m);
        }
        let mut req = self.http.post(format!("{}/v1/chat/completions", self.base_url)).json(&body);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        let resp = req.send().await.context("Chat request failed")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("Chat request failed ({}): {}", status, text);
        }
        use futures::StreamExt;
        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut finish = String::from("stop");
        while let Some(chunk) = stream.next().await {
            if should_stop() {
                bail!("aborted");
            }
            let bytes = chunk.context("Stream read failed")?;
            buf.push_str(&String::from_utf8_lossy(bytes.as_ref()));
            while let Some(pos) = buf.find('\n') {
                let line: String = buf.drain(..=pos).collect();
                let line = line.trim();
                if line.is_empty() || line.starts_with(':') {
                    continue;
                }
                if let Some(ev) = parse_sse_line(line) {
                    match ev {
                        StreamEvent::Finish { reason } => {
                            finish = reason;
                            on_event(StreamEvent::Finish { reason: finish.clone() });
                            return Ok(finish);
                        }
                        ev => on_event(ev),
                    }
                }
            }
        }
        on_event(StreamEvent::Finish { reason: finish.clone() });
        Ok(finish)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_content_delta() {
        let line = r#"data: {"choices":[{"delta":{"content":"Hi"}}]}"#;
        assert_eq!(parse_sse_line(line), Some(StreamEvent::Content { text: "Hi".into() }));
    }

    #[test]
    fn parses_done_sentinel() {
        assert_eq!(
            parse_sse_line("data: [DONE]"),
            Some(StreamEvent::Finish { reason: "stop".into() })
        );
    }

    #[test]
    fn ignores_empty_content_delta() {
        let line = r#"data: {"choices":[{"delta":{"content":""}}]}"#;
        assert_eq!(parse_sse_line(line), None);
    }

    #[test]
    fn parses_tool_call_delta() {
        let line = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read_file","arguments":"{\"pa"}}]}}]}"#;
        match parse_sse_line(line) {
            Some(StreamEvent::ToolCallDelta { index, id, name, arguments_delta }) => {
                assert_eq!(index, 0);
                assert_eq!(id.as_deref(), Some("call_1"));
                assert_eq!(name.as_deref(), Some("read_file"));
                assert_eq!(arguments_delta, r#"{"pa"#);
            }
            other => panic!("wrong event: {other:?}"),
        }
    }

    #[test]
    fn parses_finish_reason() {
        let line = r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#;
        assert_eq!(
            parse_sse_line(line),
            Some(StreamEvent::Finish { reason: "tool_calls".into() })
        );
    }

    #[test]
    fn collector_assembles_and_repairs_args() {
        let mut collector = StreamCollector::default();
        collector.push(&StreamEvent::ToolCallDelta {
            index: 0,
            id: Some("call_1".into()),
            name: Some("read_file".into()),
            arguments_delta: r#"{"path": "a.tx"#.into(),
        });
        collector.push(&StreamEvent::ToolCallDelta {
            index: 0,
            id: None,
            name: None,
            arguments_delta: r#"t"}"#.into(),
        });
        let calls = collector.finish().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "read_file");
        assert_eq!(calls[0].function.arguments, r#"{"path": "a.txt"}"#);
    }

    #[test]
    fn empty_arguments_become_empty_object() {
        let mut collector = StreamCollector::default();
        collector.push(&StreamEvent::ToolCallDelta {
            index: 0,
            id: Some("x".into()),
            name: Some("bash".into()),
            arguments_delta: String::new(),
        });
        let calls = collector.finish().unwrap();
        assert_eq!(calls[0].function.arguments, "{}");
    }

    #[test]
    fn unparseable_arguments_fail_loud() {
        let mut collector = StreamCollector::default();
        collector.push(&StreamEvent::ToolCallDelta {
            index: 0,
            id: Some("x".into()),
            name: Some("t".into()),
            arguments_delta: "not json at all".into(),
        });
        assert!(collector.finish().is_err());
    }

    #[test]
    fn non_data_lines_are_ignored() {
        assert_eq!(parse_sse_line("event: message"), None);
        assert_eq!(parse_sse_line(""), None);
        assert_eq!(parse_sse_line(": keep-alive"), None);
    }
}
