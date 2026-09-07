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
    /// String content or a multimodal parts array (text + image_url) for
    /// vision models. JSON so both shapes serialize transparently.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: Some(Value::String(content.into())),
            tool_calls: None,
            tool_call_id: None,
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: Some(Value::String(content.into())),
            tool_calls: None,
            tool_call_id: None,
        }
    }
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: Some(Value::String(content.into())),
            tool_calls: None,
            tool_call_id: None,
        }
    }
}

// ── Streaming events ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    /// Incremental assistant text.
    Content { text: String },
    /// Incremental model reasoning (`reasoning_content` deltas; shown as a
    /// collapsed "Thinking…" block in the UI, excluded from answer metrics).
    ReasoningDelta { text: String },
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
    /// Usage stats (arrives in the final chunk when the server supports
    /// `stream_options.include_usage`).
    Usage { prompt_tokens: u64, completion_tokens: u64 },
    /// Non-fatal status for the user (e.g. "model is loading, first response
    /// may be slow").
    Notice { text: String },
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

// ── Router API (llama.cpp router mode) ──────────────────────────────────────

/// One entry of the router's `GET /models` list.
#[derive(Debug, Clone, PartialEq)]
pub struct RouterModel {
    pub id: String,
    /// `loading` | `loaded` | `unloaded` | `failed`
    pub status: String,
}

/// Parse the OAI-compatible `/models` payload (router mode). Status is nested:
/// `data[i].status.value`.
pub fn parse_router_models(json_text: &str) -> Vec<RouterModel> {
    let Ok(v) = serde_json::from_str::<Value>(json_text) else {
        return Vec::new();
    };
    let Some(entries) = v.get("data").and_then(|d| d.as_array()) else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|e| {
            let id = e.get("id")?.as_str()?.to_string();
            let status = e
                .get("status")
                .and_then(|s| s.get("value"))
                .and_then(|s| s.as_str())
                .unwrap_or("unknown")
                .to_string();
            Some(RouterModel { id, status })
        })
        .collect()
}

impl LlmClient {
    /// List models registered with the router (`GET /models`).
    pub async fn router_models(&self) -> Result<Vec<RouterModel>> {
        let resp = self
            .http
            .get(format!("{}/models", self.base_url))
            .send()
            .await
            .context("Router models request failed")?;
        let resp = resp.error_for_status()?;
        let text = resp.text().await?;
        Ok(parse_router_models(&text))
    }

    /// Force the router to re-read its models preset (`GET /models?reload=1`).
    pub async fn router_reload(&self) -> Result<()> {
        let resp = self
            .http
            .get(format!("{}/models", self.base_url))
            .query(&[("reload", "1")])
            .send()
            .await
            .context("Router reload request failed")?;
        resp.error_for_status()?;
        Ok(())
    }

    /// Load a registered model on demand (`POST /models/load`).
    pub async fn router_load(&self, name: &str) -> Result<()> {
        let resp = self
            .http
            .post(format!("{}/models/load", self.base_url))
            .json(&serde_json::json!({ "model": name }))
            .send()
            .await
            .context("Router load request failed")?;
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            bail!("Loading model '{}' failed: {}", name, text);
        }
        // Note: the load request returns immediately; the model loads in the
        // background (child process spawn + GGUF load).
        Ok(())
    }

    /// Unload a model (frees its VRAM; LRU eviction also happens server-side).
    #[allow(dead_code)]
    pub async fn router_unload(&self, name: &str) -> Result<()> {
        let resp = self
            .http
            .post(format!("{}/models/unload", self.base_url))
            .json(&serde_json::json!({ "model": name }))
            .send()
            .await
            .context("Router unload request failed")?;
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            bail!("Unloading model '{}' failed: {}", name, text);
        }
        Ok(())
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
    if let Some(usage) = v.get("usage").filter(|u| u.is_object()) {
        if let (Some(p), Some(c)) = (
            usage.get("prompt_tokens").and_then(|t| t.as_u64()),
            usage.get("completion_tokens").and_then(|t| t.as_u64()),
        ) {
            return Some(StreamEvent::Usage { prompt_tokens: p, completion_tokens: c });
        }
    }
    let choice = v.get("choices")?.get(0)?;
    let delta = choice.get("delta")?;
    if let Some(text) = delta.get("reasoning_content").and_then(|c| c.as_str()) {
        if !text.is_empty() {
            return Some(StreamEvent::ReasoningDelta { text: text.to_string() });
        }
    }
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
    /// abort (the caller owns the flag). `tools` attaches OpenAI function
    /// schemas for tool calling.
    pub async fn chat_stream(
        &self,
        model: Option<&str>,
        messages: &[ChatMessage],
        tools: Option<&[Value]>,
        reasoning_effort: Option<&str>,
        should_stop: impl Fn() -> bool,
        mut on_event: impl FnMut(StreamEvent),
    ) -> Result<String> {
        let mut body = serde_json::json!({
            "messages": messages,
            "stream": true,
            "stream_options": { "include_usage": true },
        });
        if let Some(m) = model {
            body["model"] = Value::from(m);
        }
        if let Some(effort) = reasoning_effort {
            body["reasoning_effort"] = Value::from(effort);
        }
        if let Some(t) = tools {
            body["tools"] = Value::from(t.to_vec());
        }
        // A router-mode server answers 503 ("Loading model!") while the role
        // model is still loading — wait for it instead of failing the turn.
        // The notice is emitted once; polling continues silently.
        let mut noticed_loading = false;
        let load_deadline = std::time::Instant::now() + std::time::Duration::from_secs(900);
        let resp = loop {
            let r = self
                .http
                .post(format!("{}/v1/chat/completions", self.base_url))
                .json(&body);
            let mut r = r;
            if let Some(key) = &self.api_key {
                r = r.bearer_auth(key);
            }
            let resp = r.send().await.context("Chat request failed")?;
            if resp.status() == reqwest::StatusCode::SERVICE_UNAVAILABLE {
                if std::time::Instant::now() >= load_deadline {
                    let text = resp.text().await.unwrap_or_default();
                    bail!("Chat request failed (503): {}", text);
                }
                // A model that failed to load will 503 forever — bail now with
                // a clear error instead of spinning until the deadline.
                if let Some(name) = model {
                    if let Ok(list) = self.router_models().await {
                        if let Some(entry) = list.iter().find(|e| e.id == name) {
                            if entry.status == "failed" {
                                bail!("Model '{name}' failed to load — check Server Logs for the child error");
                            }
                        }
                    }
                }
                if !noticed_loading {
                    noticed_loading = true;
                    on_event(StreamEvent::Notice {
                        text: "Model is loading — this may take a while; your message will be answered as soon as it is ready.".to_string(),
                    });
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                continue;
            }
            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                bail!("Chat request failed ({}): {}", status, text);
            }
            break resp;
        };
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
    fn parses_router_models_payload() {
        let payload = r#"{"object":"list","data":[
            {"id":"KAT-Coder","aliases":[],"object":"model","owned_by":"llamacpp","created":1,
             "status":{"value":"loaded","args":[]},"source":"preset","can_remove":true},
            {"id":"gemma-4-4b","status":{"value":"unloaded"},"source":"preset"}
        ]}"#;
        let models = parse_router_models(payload);
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "KAT-Coder");
        assert_eq!(models[0].status, "loaded");
        assert_eq!(models[1].id, "gemma-4-4b");
        assert_eq!(models[1].status, "unloaded");
    }

    #[test]
    fn router_models_invalid_payload_is_empty() {
        assert!(parse_router_models("not json").is_empty());
        assert!(parse_router_models(r#"{"data":null}"#).is_empty());
    }

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
