//! Anthropic Messages API provider.
//!
//! Translates the orchestrator's OpenAI-shaped chat into Anthropic's
//! `POST /v1/messages` format and streams the response back as text deltas
//! and accumulated tool calls.
//!
//! Key features:
//! - **Streaming** via SSE events (`text_delta`, `input_json_delta`, …).
//! - **Tool use** round-trip: OpenAI `tool_calls` ↔ Anthropic `tool_use`
//!   content blocks; OpenAI `role:"tool"` messages ↔ user `tool_result`
//!   content blocks.
//! - **Prompt caching** (`cache_control: ephemeral`) on the static head of
//!   the system prompt and the trailing tool definition — both stable across
//!   turns within a session.
//! - **Retries** with jittered exponential backoff on 5xx / 529 / network
//!   errors; on final failure the provider transparently swaps to
//!   `fallback_model` for one more attempt.

use super::{ChatRequest, ChatRespMessage, DeltaTx, LlmProvider, PingInfo};
use crate::chat::{FunctionCall, ToolCall};
use crate::error::{Error, Result};
use async_trait::async_trait;
use futures::StreamExt;
use rand::Rng;
use serde_json::{json, Value};
use std::time::Duration;

/// Strip model-emitted reasoning blocks (`` … ``
/// and Qwen's `<|thinking|>` … `<|/thinking|>`) from a text fragment.
///
/// Many OpenAI-compatible providers (Qwen, DeepSeek, Kimi, GLM, …) emit
/// chain-of-thought as raw `delta.content` text instead of a separate
/// `reasoning_content` channel. Without this filter, that prose leaks
/// verbatim into the chat bubble and into the persisted assistant message —
/// which then re-feeds itself on the next turn and reinforces the behaviour,
/// making the model *look* as if it had no system prompt.
///
/// We strip both flavours at the streaming boundary so the deltas the user
/// sees, the assembled final text, and the round-tripped history all stay
/// free of reasoning prose. Returns the (possibly empty) visible text.
pub fn strip_reasoning(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }
    if !s.contains("<think>") && !s.contains("<|thinking|>") {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        let open_a = rest.find("<think>");
        let open_b = rest.find("<|thinking|>");
        let (open_pos, open_len, close_marker) = match (open_a, open_b) {
            (Some(a), Some(b)) if a <= b => (a, "<think>".len(), "</think>"),
            (Some(_a), Some(b)) => (b, "<|thinking|>".len(), "<|/thinking|>"),
            (Some(a), None) => (a, "<think>".len(), "</think>"),
            (None, Some(b)) => (b, "<|thinking|>".len(), "<|/thinking|>"),
            (None, None) => break,
        };
        out.push_str(&rest[..open_pos]);
        rest = &rest[open_pos + open_len..];
        match rest.find(close_marker) {
            Some(close) => rest = &rest[close + close_marker.len()..],
            None => {
                // Unterminated reasoning block — drop the rest. A model that
                // opens one and forgets to close it is streaming a
                // half-reasoning payload; better to truncate the visible
                // bubble than to leak the rest of the turn.
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod reasoning_tests {
    use super::strip_reasoning;

    #[test]
    fn passthrough_when_no_marker() {
        assert_eq!(strip_reasoning("hello world"), "hello world");
    }

    #[test]
    fn strips_think_block() {
        assert_eq!(
            strip_reasoning("before<think>secret reasoning</think>after"),
            "beforeafter"
        );
    }

    #[test]
    fn strips_qwen_thinking_block() {
        assert_eq!(strip_reasoning("a<|thinking|>x<|/thinking|>b"), "ab");
    }

    #[test]
    fn drops_unterminated_block() {
        assert_eq!(strip_reasoning("safe<think>lost"), "safe");
    }

    #[test]
    fn keeps_multiple_outside_thinks() {
        assert_eq!(
            strip_reasoning("x<think>1</think> y<think>2</think> z"),
            "x y z"
        );
    }
}

/// Anthropic API version pinned for compatibility. Bump intentionally.
const API_VERSION: &str = "2023-06-01";

/// Marker used by [`crate::orchestrator::Orchestrator::build_system_prompt`]
/// to separate the static head from per-turn dynamic state. Splitting on it
/// lets us mark only the head as cacheable.
const WORLD_STATE_MARKER: &str = "\n\n[WORLD STATE]\n";

pub struct AnthropicProvider {
    base_url: String,
    primary_model: String,
    fallback_model: Option<String>,
    cache: bool,
    #[allow(dead_code)]
    max_tokens: u32,
    api_key: Option<String>,
    http: reqwest::Client,
    label: String,
}

impl AnthropicProvider {
    pub fn new(
        base_url: String,
        primary_model: String,
        fallback_model: Option<String>,
        cache: bool,
        max_tokens: u32,
        api_key: Option<String>,
    ) -> Self {
        Self {
            base_url,
            primary_model,
            fallback_model,
            cache,
            max_tokens,
            api_key,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(180))
                .build()
                .expect("reqwest client"),
            label: "anthropic".into(),
        }
    }

    fn endpoint(&self) -> String {
        format!("{}/v1/messages", self.base_url.trim_end_matches('/'))
    }

    /// Build the JSON body for one streaming request against `model`.
    fn build_body(&self, model: &str, req: &ChatRequest) -> Result<Value> {
        let (system_blocks, body_messages) =
            translate_messages(&req.messages, req.cache && self.cache)?;
        let mut body = json!({
            "model": model,
            "max_tokens": req.max_tokens.max(1),
            "messages": body_messages,
            "stream": true,
            "temperature": req.temperature,
        });
        if let Some(s) = system_blocks {
            body["system"] = s;
        }
        if let Some(tools) = &req.tools {
            body["tools"] = translate_tools(tools, req.cache && self.cache)?;
            body["tool_choice"] = json!({ "type": "auto" });
        }
        Ok(body)
    }

    /// One streaming attempt — no retry, no fallback.
    async fn stream_once(
        &self,
        model: &str,
        req: &ChatRequest,
        delta_tx: &DeltaTx,
    ) -> Result<ChatRespMessage> {
        let api_key = self.api_key.as_deref().ok_or_else(|| {
            Error::Orchestrator("anthropic: missing API key (set ANTHROPIC_API_KEY)".into())
        })?;

        let body = self.build_body(model, req)?;
        tracing::debug!("anthropic -> {}: model={}", self.endpoint(), model);

        let resp = self
            .http
            .post(self.endpoint())
            .header("x-api-key", api_key)
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Orchestrator(format!("anthropic send: {}", e)))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            tracing::error!("anthropic {} body={}", status, truncate(&text, 500));
            // Distinguish retryable errors so the caller can retry/fallback.
            return Err(Error::Orchestrator(format!(
                "anthropic {} -> {}",
                status,
                truncate(&text, 500)
            )));
        }

        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut parser = StreamState::default();

        while let Some(chunk) = stream.next().await {
            let bytes =
                chunk.map_err(|e| Error::Orchestrator(format!("anthropic stream: {}", e)))?;
            buf.push_str(&String::from_utf8_lossy(&bytes));
            // SSE events are separated by blank lines.
            while let Some(end) = buf.find("\n\n") {
                let event_block: String = buf.drain(..end + 2).collect();
                process_event_block(&event_block, &mut parser, &mut |d: &str| {
                    let _ = delta_tx.send(d.to_string());
                });
                if parser.done {
                    break;
                }
            }
            if parser.done {
                break;
            }
        }

        Ok(parser.into_response())
    }

    /// Apply jittered exponential backoff and (optionally) a final fallback
    /// model swap. Non-retryable errors (e.g. 4xx auth) propagate immediately.
    async fn stream_with_retry(
        &self,
        req: &ChatRequest,
        delta_tx: &DeltaTx,
    ) -> Result<ChatRespMessage> {
        let mut last_err: Option<Error> = None;

        // Primary model with retries.
        for attempt in 0..3 {
            match self.stream_once(&self.primary_model, req, delta_tx).await {
                Ok(v) => return Ok(v),
                Err(e) => {
                    if !is_retryable(&e) {
                        return Err(e);
                    }
                    tracing::warn!("anthropic attempt {} failed: {}", attempt + 1, e);
                    last_err = Some(e);
                    backoff(attempt).await;
                }
            }
        }

        // Final fallback to secondary model — one extra attempt.
        if let Some(fb) = &self.fallback_model {
            tracing::warn!(
                "anthropic: primary {} exhausted; falling back to {}",
                self.primary_model,
                fb
            );
            match self.stream_once(fb, req, delta_tx).await {
                Ok(v) => return Ok(v),
                Err(e) => {
                    return Err(Error::Orchestrator(format!(
                        "anthropic: primary failed ({}); fallback {} also failed: {}",
                        last_err
                            .as_ref()
                            .map(|e| e.to_string())
                            .unwrap_or_else(|| "unknown".into()),
                        fb,
                        e
                    )));
                }
            }
        }

        Err(last_err.unwrap_or_else(|| Error::Orchestrator("anthropic: unknown failure".into())))
    }
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    fn label(&self) -> &str {
        &self.label
    }

    fn primary_model(&self) -> &str {
        &self.primary_model
    }

    fn fallback_model(&self) -> Option<&str> {
        self.fallback_model.as_deref()
    }

    async fn chat_stream(&self, req: ChatRequest, delta_tx: DeltaTx) -> Result<ChatRespMessage> {
        self.stream_with_retry(&req, &delta_tx).await
    }

    async fn ping(&self) -> Result<PingInfo> {
        if self.api_key.is_none() {
            return Ok(PingInfo {
                provider: self.label.clone(),
                model: self.primary_model.clone(),
                ok: false,
                note: Some("missing API key (ANTHROPIC_API_KEY)".into()),
            });
        }
        let req = ChatRequest {
            model: self.primary_model.clone(),
            messages: vec![json!({"role": "user", "content": "ping"})],
            tools: None,
            temperature: 0.0,
            max_tokens: 8,
            cache: false,
        };
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        match self.stream_once(&self.primary_model, &req, &tx).await {
            Ok(_) => Ok(PingInfo {
                provider: self.label.clone(),
                model: self.primary_model.clone(),
                ok: true,
                note: None,
            }),
            Err(e) => Ok(PingInfo {
                provider: self.label.clone(),
                model: self.primary_model.clone(),
                ok: false,
                note: Some(e.to_string()),
            }),
        }
    }
}

// ---------------- Translation ----------------

/// Convert an OpenAI-shaped messages array into Anthropic's
/// `(system, messages)` pair. Caching: when `cache=true`, the static head of
/// the system prompt (everything up to `WORLD_STATE_MARKER`) is wrapped as a
/// cache-controlled content block.
pub fn translate_messages(messages: &[Value], cache: bool) -> Result<(Option<Value>, Vec<Value>)> {
    let mut system: Option<String> = None;
    let mut out: Vec<Value> = Vec::with_capacity(messages.len());

    for m in messages {
        let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("");
        match role {
            "system" => {
                let text = string_content(m.get("content"));
                // Concatenate any duplicate system messages.
                system = Some(match system.take() {
                    Some(prev) => format!("{}\n\n{}", prev, text),
                    None => text,
                });
            }
            "user" => {
                let blocks = json!([{
                    "type": "text",
                    "text": string_content(m.get("content")),
                }]);
                out.push(json!({"role": "user", "content": blocks}));
            }
            "assistant" => {
                let mut blocks: Vec<Value> = Vec::new();
                let text = string_content(m.get("content"));
                if !text.is_empty() {
                    blocks.push(json!({"type": "text", "text": text}));
                }
                if let Some(tcs) = m.get("tool_calls").and_then(|v| v.as_array()) {
                    for tc in tcs {
                        let id = tc
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let func = tc.get("function").cloned().unwrap_or(Value::Null);
                        let name = func
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let raw_args = func
                            .get("arguments")
                            .and_then(|v| v.as_str())
                            .unwrap_or("{}");
                        let input: Value =
                            serde_json::from_str(raw_args).unwrap_or_else(|_| json!({}));
                        blocks.push(json!({
                            "type": "tool_use",
                            "id": id,
                            "name": name,
                            "input": input,
                        }));
                    }
                }
                if blocks.is_empty() {
                    // Anthropic rejects empty assistant turns.
                    blocks.push(json!({"type": "text", "text": ""}));
                }
                out.push(json!({"role": "assistant", "content": blocks}));
            }
            "tool" => {
                let tool_use_id = m
                    .get("tool_call_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let content = string_content(m.get("content"));
                out.push(json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": tool_use_id,
                        "content": content,
                    }]
                }));
            }
            _ => {
                tracing::debug!("anthropic: dropping unknown role {:?}", role);
            }
        }
    }

    let system_value = system.map(|s| {
        if !cache {
            return json!(s);
        }
        if let Some((head, tail)) = split_system(&s) {
            // Two content blocks: cached static head + uncached dynamic tail.
            json!([
                {
                    "type": "text",
                    "text": head,
                    "cache_control": { "type": "ephemeral" },
                },
                { "type": "text", "text": tail },
            ])
        } else {
            // No marker — cache the whole thing as one block.
            json!([
                {
                    "type": "text",
                    "text": s,
                    "cache_control": { "type": "ephemeral" },
                }
            ])
        }
    });

    Ok((system_value, out))
}

/// Convert OpenAI tool definitions into Anthropic's shape. When `cache=true`,
/// the trailing tool gets `cache_control: ephemeral` — Anthropic caches up
/// to and including that block, covering the entire tool catalog.
pub fn translate_tools(tools: &[Value], cache: bool) -> Result<Value> {
    let mut out: Vec<Value> = Vec::with_capacity(tools.len());
    for t in tools {
        let func = t.get("function").cloned().unwrap_or_else(|| t.clone());
        let name = func
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let description = func
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let input_schema = func
            .get("parameters")
            .cloned()
            .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
        out.push(json!({
            "name": name,
            "description": description,
            "input_schema": input_schema,
        }));
    }
    if cache {
        if let Some(last) = out.last_mut() {
            if let Some(obj) = last.as_object_mut() {
                obj.insert("cache_control".into(), json!({"type": "ephemeral"}));
            }
        }
    }
    Ok(Value::Array(out))
}

fn split_system(s: &str) -> Option<(String, String)> {
    s.find(WORLD_STATE_MARKER)
        .map(|idx| (s[..idx].to_string(), s[idx..].to_string()))
}

fn string_content(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).chain("…".chars()).collect()
    }
}

fn is_retryable(e: &Error) -> bool {
    let msg = e.to_string().to_lowercase();
    // 5xx, 529 overloaded, request timeouts, transient network errors.
    msg.contains("anthropic 5")
        || msg.contains("anthropic 529")
        || msg.contains("overloaded")
        || msg.contains("timed out")
        || msg.contains("timeout")
        || msg.contains("connection")
        || msg.contains("dns")
        || msg.contains("send")
        || msg.contains("stream")
}

async fn backoff(attempt: u32) {
    let base_ms = match attempt {
        0 => 250u64,
        1 => 500,
        _ => 1000,
    };
    let jitter: f64 = rand::thread_rng().gen_range(0.5..1.5);
    let delay = (base_ms as f64 * jitter) as u64;
    tokio::time::sleep(Duration::from_millis(delay)).await;
}

// ---------------- SSE parsing ----------------

#[derive(Default)]
struct StreamState {
    text: String,
    blocks: Vec<BlockAccum>,
    done: bool,
}

#[derive(Default)]
struct BlockAccum {
    kind: String, // "text" | "tool_use"
    id: String,
    name: String,
    text: String,       // for text blocks
    input_json: String, // accumulated partial_json for tool_use
}

impl StreamState {
    fn ensure_block(&mut self, idx: usize) -> &mut BlockAccum {
        while self.blocks.len() <= idx {
            self.blocks.push(BlockAccum::default());
        }
        &mut self.blocks[idx]
    }

    fn into_response(self) -> ChatRespMessage {
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        // Defensive: strip any reasoning blocks that were only fully
        // visible after the final delta landed. Cheap re-pass — usually
        // a no-op because the streaming path already filtered.
        let mut text = strip_reasoning(&self.text);

        for b in self.blocks {
            match b.kind.as_str() {
                "tool_use" => {
                    let arguments = if b.input_json.trim().is_empty() {
                        "{}".to_string()
                    } else {
                        b.input_json
                    };
                    tool_calls.push(ToolCall {
                        id: b.id,
                        kind: "function".into(),
                        function: FunctionCall {
                            name: b.name,
                            arguments,
                        },
                    });
                }
                "text" => {
                    // Most providers stream text via the top-level `text`
                    // accumulator already; this branch covers the case where
                    // a server sends only block-level content.
                    if !b.text.is_empty() && !text.contains(&b.text) {
                        text.push_str(&b.text);
                    }
                }
                _ => {}
            }
        }

        ChatRespMessage {
            role: "assistant".into(),
            content: if text.is_empty() { None } else { Some(text) },
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
        }
    }
}

fn process_event_block<F>(block: &str, state: &mut StreamState, on_text: &mut F)
where
    F: FnMut(&str),
{
    let mut event_name: Option<&str> = None;
    let mut data_lines: Vec<&str> = Vec::new();
    for line in block.lines() {
        if let Some(name) = line.strip_prefix("event:") {
            event_name = Some(name.trim());
        } else if let Some(d) = line.strip_prefix("data:") {
            data_lines.push(d.trim_start());
        }
    }
    if data_lines.is_empty() {
        return;
    }
    let data = data_lines.join("");
    if data == "[DONE]" {
        state.done = true;
        return;
    }
    let json: Value = match serde_json::from_str(&data) {
        Ok(v) => v,
        Err(_) => return,
    };
    let typ = event_name
        .map(|s| s.to_string())
        .or_else(|| {
            json.get("type")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_default();

    match typ.as_str() {
        "message_start" | "ping" => {}
        "content_block_start" => {
            let idx = json.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let cb = json.get("content_block").cloned().unwrap_or(Value::Null);
            let kind = cb
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let entry = state.ensure_block(idx);
            entry.kind = kind.clone();
            if kind == "tool_use" {
                entry.id = cb
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                entry.name = cb
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
            }
        }
        "content_block_delta" => {
            let idx = json.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let delta = json.get("delta").cloned().unwrap_or(Value::Null);
            let dtyp = delta
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            match dtyp.as_str() {
                "text_delta" => {
                    let raw = delta.get("text").and_then(|v| v.as_str()).unwrap_or("");
                    // Strip any model-emitted reasoning so it never reaches
                    // the chat bubble or the persisted assistant message.
                    let t = strip_reasoning(raw);
                    if !t.is_empty() {
                        state.text.push_str(&t);
                        let entry = state.ensure_block(idx);
                        if entry.kind.is_empty() {
                            entry.kind = "text".into();
                        }
                        entry.text.push_str(&t);
                        on_text(&t);
                    } else if raw.contains("<think>") || raw.contains("<|thinking|>") {
                        // Pure-reasoning delta: still let it accumulate so
                        // the trimmer catches a half-opened block on the
                        // next delta, but emit nothing to the UI.
                        state.text.push_str(raw);
                    }
                }
                "input_json_delta" => {
                    let pj = delta
                        .get("partial_json")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let entry = state.ensure_block(idx);
                    if entry.kind.is_empty() {
                        entry.kind = "tool_use".into();
                    }
                    entry.input_json.push_str(pj);
                }
                _ => {}
            }
        }
        "content_block_stop" => {}
        "message_delta" => {}
        "message_stop" => {
            state.done = true;
        }
        "error" => {
            tracing::error!("anthropic error event: {}", data);
            state.done = true;
        }
        _ => {}
    }
}

// ---------------- Tests ----------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translate_simple_user_message() {
        let msgs = vec![json!({"role": "user", "content": "hello"})];
        let (system, out) = translate_messages(&msgs, false).unwrap();
        assert!(system.is_none());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["role"], "user");
        assert_eq!(out[0]["content"][0]["type"], "text");
        assert_eq!(out[0]["content"][0]["text"], "hello");
    }

    #[test]
    fn translate_lifts_system_to_top_level() {
        let msgs = vec![
            json!({"role": "system", "content": "you are a bot"}),
            json!({"role": "user", "content": "hi"}),
        ];
        let (system, out) = translate_messages(&msgs, false).unwrap();
        assert_eq!(system.unwrap(), json!("you are a bot"));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["role"], "user");
    }

    #[test]
    fn translate_caches_static_system_head() {
        let s = format!("stable head{}dynamic tail", super::WORLD_STATE_MARKER);
        let msgs = vec![
            json!({"role": "system", "content": s}),
            json!({"role": "user", "content": "hi"}),
        ];
        let (system, _) = translate_messages(&msgs, true).unwrap();
        let arr = system.unwrap();
        let arr = arr.as_array().expect("system blocks");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["text"], "stable head");
        assert_eq!(arr[0]["cache_control"]["type"], "ephemeral");
        assert!(arr[1]["text"].as_str().unwrap().contains("dynamic tail"));
        assert!(arr[1].get("cache_control").is_none());
    }

    #[test]
    fn translate_assistant_with_tool_calls() {
        let msgs = vec![json!({
            "role": "assistant",
            "content": "Calling tool",
            "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": {
                    "name": "list_workspaces",
                    "arguments": "{}",
                }
            }]
        })];
        let (_, out) = translate_messages(&msgs, false).unwrap();
        let blocks = out[0]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[1]["type"], "tool_use");
        assert_eq!(blocks[1]["id"], "call_1");
        assert_eq!(blocks[1]["name"], "list_workspaces");
        assert!(blocks[1]["input"].is_object());
    }

    #[test]
    fn translate_tool_role_to_tool_result() {
        let msgs = vec![json!({
            "role": "tool",
            "tool_call_id": "call_42",
            "content": "{\"ok\":true}"
        })];
        let (_, out) = translate_messages(&msgs, false).unwrap();
        assert_eq!(out[0]["role"], "user");
        let blocks = out[0]["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "tool_result");
        assert_eq!(blocks[0]["tool_use_id"], "call_42");
        assert_eq!(blocks[0]["content"], "{\"ok\":true}");
    }

    #[test]
    fn translate_tools_basic() {
        let tools = vec![json!({
            "type": "function",
            "function": {
                "name": "list_workspaces",
                "description": "list workspaces",
                "parameters": {"type": "object", "properties": {}}
            }
        })];
        let out = translate_tools(&tools, true).unwrap();
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"], "list_workspaces");
        assert_eq!(arr[0]["description"], "list workspaces");
        assert_eq!(arr[0]["input_schema"]["type"], "object");
        assert_eq!(arr[0]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn sse_text_delta_round_trip() {
        let mut state = StreamState::default();
        let mut captured = String::new();
        let mut sink = |s: &str| captured.push_str(s);

        let events = [
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{}}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" world\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        ];
        for e in events {
            process_event_block(e, &mut state, &mut sink);
        }
        let resp = state.into_response();
        assert_eq!(resp.content.as_deref(), Some("Hello world"));
        assert!(resp.tool_calls.is_none());
        assert_eq!(captured, "Hello world");
    }

    #[test]
    fn sse_tool_use_round_trip() {
        let mut state = StreamState::default();
        let mut sink = |_: &str| {};

        let events = [
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"list_workspaces\",\"input\":{}}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"name\\\":\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\" \\\"alpha\\\"}\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        ];
        for e in events {
            process_event_block(e, &mut state, &mut sink);
        }
        let resp = state.into_response();
        let tcs = resp.tool_calls.expect("tool calls");
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].id, "toolu_1");
        assert_eq!(tcs[0].function.name, "list_workspaces");
        // Arguments parse as valid JSON with the expected key.
        let parsed: Value = serde_json::from_str(&tcs[0].function.arguments).unwrap();
        assert_eq!(parsed["name"], "alpha");
    }

    #[test]
    fn is_retryable_classifies_errors() {
        assert!(is_retryable(&Error::Orchestrator(
            "anthropic 529 -> overloaded".into()
        )));
        assert!(is_retryable(&Error::Orchestrator(
            "anthropic 502 -> bad gateway".into()
        )));
        assert!(is_retryable(&Error::Orchestrator(
            "anthropic stream: connection reset".into()
        )));
        assert!(!is_retryable(&Error::Orchestrator(
            "anthropic 401 -> unauthorized".into()
        )));
        assert!(!is_retryable(&Error::Orchestrator(
            "anthropic 400 -> invalid model".into()
        )));
    }
}
