//! OpenAI-compatible SSE client (llama.cpp `/v1/chat/completions` first).

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
    /// String or multimodal parts array (text + image_url) for vision.
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
    Content { text: String },
    /// Reasoning deltas; excluded from answer text and speed metrics.
    ReasoningDelta { text: String },
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
    /// Usage stats; sent in the final chunk when the server supports it.
    Usage { prompt_tokens: u64, completion_tokens: u64 },
    Notice { text: String },
}

/// Assembles deltas into complete tool calls; args repaired via `jsonfix`.
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

    /// Repaired args; still fails loud when unparseable.
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

/// Parse `/models`; status lives at `data[i].status.value`.
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
        // Load returns immediately; the model loads in the background.
        Ok(())
    }

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

    pub async fn slot_context(&self) -> Result<Option<u64>> {
        self.slot_context_for(None).await
    }

    /// Largest slot `n_ctx`; router mode scopes to one model (`?model=`).
    pub async fn slot_context_for(&self, model: Option<&str>) -> Result<Option<u64>> {
        Ok(self.slot_fill_for(model).await?.map(|(ctx, _)| ctx))
    }

    pub async fn slot_fill(&self) -> Result<Option<(u64, u64)>> {
        self.slot_fill_for(None).await
    }

    /// Largest `(n_ctx, prompt + generated)` across slots; router mode scopes
    /// to one model (`?model=`), otherwise the router would fan out to children.
    pub async fn slot_fill_for(&self, model: Option<&str>) -> Result<Option<(u64, u64)>> {
        let mut req = self.http.get(format!("{}/slots", self.base_url));
        if let Some(m) = model {
            req = req.query(&[("model", m)]);
        }
        let resp = req.send().await.context("Slots request failed")?;
        if !resp.status().is_success() {
            return Ok(None);
        }
        let text = resp.text().await?;
        Ok(max_slot_fill(&text))
    }

    /// Runtime context size from `/props` (`default_generation_settings.n_ctx`);
    /// the configured server value, not the model's training limit.
    pub async fn props_context(&self) -> Result<Option<u64>> {
        let resp = self
            .http
            .get(format!("{}/props", self.base_url))
            .send()
            .await
            .context("Props request failed")?;
        if !resp.status().is_success() {
            return Ok(None);
        }
        let text = resp.text().await?;
        Ok(parse_props_n_ctx(&text))
    }

    /// Throughput gauges from `GET /metrics`; router mode needs `?model=<id>`.
    pub async fn server_throughput(&self, router_model: Option<&str>) -> Result<ServerThroughput> {
        let mut req = self.http.get(format!("{}/metrics", self.base_url));
        if let Some(model) = router_model {
            req = req.query(&[("model", model)]);
        }
        let resp = req.send().await.context("Metrics request failed")?;
        if !resp.status().is_success() {
            return Ok(ServerThroughput::default());
        }
        let text = resp.text().await?;
        Ok(parse_throughput(&text))
    }
}

/// Throughput averages (tokens/s) from `GET /metrics`; lifetime, not per-request.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ServerThroughput {
    /// `llamacpp:prompt_tokens_seconds` — prefill rate.
    pub prompt_tps: Option<f64>,
    /// `llamacpp:predicted_tokens_seconds` — generation rate.
    pub gen_tps: Option<f64>,
    /// Live context-size gauge when the build exposes one.
    pub context_size: Option<u64>,
}

/// Parse Prometheus gauges; names normalized across llama.cpp builds.
pub fn parse_throughput(text: &str) -> ServerThroughput {
    let mut out = ServerThroughput::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let (name, value) = match (parts.next(), parts.next()) {
            (Some(n), Some(v)) => (n, v),
            _ => continue,
        };
        // Labelled per-model counters carry no plain gauge; skip them.
        if name.contains('{') {
            continue;
        }
        let norm = name.replace(':', "_").to_lowercase();
        let short = norm.strip_prefix("llamacpp_").unwrap_or(&norm);
        match short {
            "prompt_tokens_seconds" => {
                if let Ok(v) = value.parse::<f64>() {
                    if v.is_finite() {
                        out.prompt_tps = Some(v);
                    }
                }
            }
            "predicted_tokens_seconds" => {
                if let Ok(v) = value.parse::<f64>() {
                    if v.is_finite() {
                        out.gen_tps = Some(v);
                    }
                }
            }
            s if s.contains("context") || s == "n_ctx" || s == "ctx_size" => {
                if let Ok(v) = value.parse::<f64>() {
                    if v.is_finite() && v > 0.0 {
                        out.context_size = Some(v as u64);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

pub fn max_slot_n_ctx(json_text: &str) -> Option<u64> {
    let v: Value = serde_json::from_str(json_text).ok()?;
    let arr = v.as_array()?;
    arr.iter()
        .filter_map(|s| s.get("n_ctx")?.as_u64())
        .max()
}

/// `default_generation_settings.n_ctx` from `/props`: the runtime context
/// size, not the training limit (`n_ctx_train` via `/v1/models` is metadata).
pub fn parse_props_n_ctx(json_text: &str) -> Option<u64> {
    let v: Value = serde_json::from_str(json_text).ok()?;
    v.get("default_generation_settings")?
        .get("n_ctx")?
        .as_u64()
        .filter(|n| *n > 0)
}

pub fn max_slot_fill(json_text: &str) -> Option<(u64, u64)> {
    let v: Value = serde_json::from_str(json_text).ok()?;
    let arr = v.as_array()?;
    let ctx = arr.iter().filter_map(|s| s.get("n_ctx")?.as_u64()).max()?;
    let used = arr
        .iter()
        .map(|s| {
            let num = |v: &Value, k: &str| v.get(k).and_then(|n| n.as_u64()).unwrap_or(0);
            // Schemas vary across builds: `n_prompt`/`n_predicted` (old) vs
            // `n_prompt_tokens`/`next_token.n_decoded` (new). Take the larger
            // reading so nothing double-counts.
            let prompt = num(s, "n_prompt_tokens").max(num(s, "n_prompt"));
            let decoded = s
                .get("next_token")
                .map(|nt| num(nt, "n_decoded").max(num(nt, "n_predicted")))
                .unwrap_or(0);
            (prompt + decoded.max(num(s, "n_predicted"))).max(num(s, "n_tokens"))
        })
        .max()
        .unwrap_or(0);
    Some((ctx, used))
}

// ── Client ──────────────────────────────────────────────────────────────────

pub struct LlmClient {
    base_url: String,
    api_key: Option<String>,
    http: reqwest::Client,
}

/// Parse one SSE `data:` line; `[DONE]` maps to `Finish { reason: "stop" }`.
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
            // No timeout: generations can stream for minutes.
            http: reqwest::Client::builder()
                .build()
                .expect("failed to build HTTP client"),
        }
    }

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
        // Router answers 503 while the model loads; wait instead of failing.
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
                // A failed model 503s forever; bail instead of polling to deadline.
                if let Some(name) = model {
                    if let Ok(list) = self.router_models().await {
                        if let Some(entry) = list.iter().find(|e| e.id == name) {
                            if entry.status == "failed" {
                                bail!("Model '{name}' failed to load — check Server Logs for error");
                            }
                        }
                    }
                }
                if !noticed_loading {
                    noticed_loading = true;
                    on_event(StreamEvent::Notice {
                        text: "Model is loading...".to_string(),
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
    fn parses_throughput_gauges() {
        let payload = "# HELP llamacpp:prompt_tokens_seconds Average prompt throughput.\n\
            # TYPE llamacpp:prompt_tokens_seconds gauge\n\
            llamacpp:prompt_tokens_seconds 197.368\n\
            llamacpp:predicted_tokens_seconds 33.4448\n\
            llamacpp:tokens_predicted_total 10\n";
        let t = parse_throughput(payload);
        assert_eq!(t.prompt_tps, Some(197.368));
        assert_eq!(t.gen_tps, Some(33.4448));
    }

    #[test]
    fn throughput_ignores_labels_and_garbage() {
        let payload = "llamacpp:predicted_tokens_seconds{model=\"x\"} 10\nnot-a-metric\n";
        let t = parse_throughput(payload);
        assert_eq!(t, ServerThroughput::default());
        assert_eq!(parse_throughput(""), ServerThroughput::default());
    }

    #[test]
    fn throughput_accepts_underscore_names_and_context_gauge() {
        let payload = "llamacpp_predicted_tokens_seconds 33.5\nllama_server_context_size 32768\n";
        let t = parse_throughput(payload);
        assert_eq!(t.gen_tps, Some(33.5));
        assert_eq!(t.context_size, Some(32768));
        assert_eq!(t.prompt_tps, None);
    }

    #[test]
    fn max_slot_fill_reads_ceiling_and_used() {
        let payload = r#"[
            {"id":0,"n_ctx":65536,"n_prompt":100,"n_predicted":20},
            {"id":1,"n_ctx":32768,"n_prompt":500}
        ]"#;
        assert_eq!(max_slot_fill(payload), Some((65536, 500)));
        assert_eq!(max_slot_fill("[]"), None);
        assert_eq!(max_slot_fill("not json"), None);
    }

    #[test]
    fn max_slot_fill_reads_current_schema() {
        // Newer builds: n_prompt_tokens + next_token.n_decoded.
        let payload = r#"[
            {"id":0,"n_ctx":65536,"is_processing":true,"n_prompt_tokens":1200,
             "next_token":{"n_decoded":300}}
        ]"#;
        assert_eq!(max_slot_fill(payload), Some((65536, 1500)));
        // Old fields still honored alongside the new ones.
        let mixed = r#"[{"n_ctx":4096,"n_prompt_tokens":100,"n_predicted":10}]"#;
        assert_eq!(max_slot_fill(mixed), Some((4096, 110)));
    }

    #[test]
    fn max_slot_n_ctx_takes_largest() {
        let payload = r#"[
            {"id":0,"id_task":135,"n_ctx":65536,"is_processing":true},
            {"id":1,"id_task":0,"n_ctx":32768,"is_processing":false},
            {"id":2,"no_ctx_here":true}
        ]"#;
        assert_eq!(max_slot_n_ctx(payload), Some(65536));
        assert_eq!(max_slot_n_ctx("[]"), None);
        assert_eq!(max_slot_n_ctx("not json"), None);
        assert_eq!(max_slot_n_ctx(r#"{"not":"an array"}"#), None);
    }

    #[test]
    fn parses_props_n_ctx() {
        let payload = r#"{"default_generation_settings":{"n_ctx":8192},"total_slots":1}"#;
        assert_eq!(parse_props_n_ctx(payload), Some(8192));
        assert_eq!(parse_props_n_ctx(r#"{"total_slots":1}"#), None);
        assert_eq!(parse_props_n_ctx(r#"{"default_generation_settings":{"n_ctx":0}}"#), None);
        assert_eq!(parse_props_n_ctx("not json"), None);
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
