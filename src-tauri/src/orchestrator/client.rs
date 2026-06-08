use crate::error::{Error, Result};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::OnceLock;

#[derive(Debug, Clone)]
pub struct OmniClient {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    http: reqwest::Client,
}

/// Shared reqwest client. `reqwest::Client` holds its own connection
/// pool + DNS cache + idle keep-alive connections; constructing one per
/// turn (which is what `build_provider` did before this fix) leaks
/// sockets and re-resolves DNS on every request. One client per process
/// is the documented best practice.
fn shared_http() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            .tcp_keepalive(std::time::Duration::from_secs(60))
            .build()
            .expect("reqwest client")
    })
}

#[derive(Debug, Deserialize)]
pub struct ChatResponse {
    pub choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
pub struct ChatChoice {
    pub message: ChatRespMessage,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ChatRespMessage {
    pub role: String,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<crate::chat::ToolCall>>,
}

impl OmniClient {
    pub fn new(base_url: String, model: String, api_key: Option<String>) -> Self {
        Self {
            base_url,
            model,
            api_key,
            http: shared_http().clone(),
        }
    }

    pub async fn chat_completions(
        &self,
        messages: Vec<Value>,
        tools: Option<Vec<Value>>,
    ) -> Result<ChatResponse> {
        let url = format!(
            "{}/v1/chat/completions",
            self.base_url.trim_end_matches('/')
        );
        let mut body = serde_json::json!({
            "model": self.model,
            "messages": messages,
            "stream": false,
            "temperature": 0.2,
        });
        if let Some(t) = tools {
            body["tools"] = Value::Array(t);
            body["tool_choice"] = Value::String("auto".into());
        }
        tracing::debug!(
            "OmniRouter -> {}: model={} messages={} stream=false",
            url,
            self.model,
            messages.len()
        );
        let mut req = self.http.post(&url).json(&body);
        if let Some(key) = &self.api_key {
            if !key.is_empty() {
                req = req.bearer_auth(key);
            }
        }
        let resp = req
            .send()
            .await
            .map_err(|e| Error::Orchestrator(format!("send: {}", e)))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            // Do NOT log the request body: it carries the full prompt
            // (chat history, world state, memory hot-cache, tool catalogue)
            // and any content the user typed. Only the status + the
            // provider's response (truncated) are recorded.
            tracing::error!(
                "OmniRouter {} response={}",
                status,
                text.chars().take(500).collect::<String>()
            );
            return Err(Error::Orchestrator(format!(
                "OmniRouter {} -> {}",
                status,
                text.chars().take(500).collect::<String>()
            )));
        }
        let parsed: ChatResponse = resp
            .json()
            .await
            .map_err(|e| Error::Orchestrator(format!("parse: {}", e)))?;
        Ok(parsed)
    }

    /// Streamed variant. `on_text_delta` fires for each text fragment as it
    /// arrives. Returns the fully-assembled assistant message at the end.
    /// Tool-call deltas are accumulated internally (not streamed to UI in
    /// MVP) and returned in the final result.
    pub async fn chat_completions_stream<F>(
        &self,
        messages: Vec<Value>,
        tools: Option<Vec<Value>>,
        mut on_text_delta: F,
    ) -> Result<ChatRespMessage>
    where
        F: FnMut(&str),
    {
        let url = format!(
            "{}/v1/chat/completions",
            self.base_url.trim_end_matches('/')
        );
        let mut body = serde_json::json!({
            "model": self.model,
            "messages": messages,
            "stream": true,
            "temperature": 0.2,
        });
        if let Some(t) = tools {
            body["tools"] = Value::Array(t);
            body["tool_choice"] = Value::String("auto".into());
        }
        let mut req = self.http.post(&url).json(&body);
        if let Some(key) = &self.api_key {
            if !key.is_empty() {
                req = req.bearer_auth(key);
            }
        }
        let resp = req
            .send()
            .await
            .map_err(|e| Error::Orchestrator(format!("send: {}", e)))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            // Do NOT log the request body — see non-stream path above.
            tracing::error!(
                "OmniRouter stream {} response={}",
                status,
                text.chars().take(500).collect::<String>()
            );
            return Err(Error::Orchestrator(format!(
                "OmniRouter {} -> {}",
                status,
                text.chars().take(500).collect::<String>()
            )));
        }

        let mut content = String::new();
        // Tool-call accumulator keyed by index.
        let mut tool_calls_by_idx: std::collections::BTreeMap<usize, AccumTool> =
            std::collections::BTreeMap::new();

        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        'outer: while let Some(chunk) = stream.next().await {
            let bytes = chunk.map_err(|e| Error::Orchestrator(format!("stream: {}", e)))?;
            buf.push_str(&String::from_utf8_lossy(&bytes));
            // SSE events are separated by blank lines.
            while let Some(end) = buf.find("\n\n") {
                let event_block: String = buf.drain(..end + 2).collect();
                for line in event_block.lines() {
                    let data = match line.strip_prefix("data:") {
                        Some(d) => d.trim_start(),
                        None => continue,
                    };
                    if data == "[DONE]" {
                        break 'outer;
                    }
                    let json: Value = match serde_json::from_str(data) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    let choice = match json.get("choices").and_then(|c| c.get(0)) {
                        Some(c) => c,
                        None => continue,
                    };
                    let delta = match choice.get("delta") {
                        Some(d) => d,
                        None => continue,
                    };
                    if let Some(text) = delta.get("content").and_then(|v| v.as_str()) {
                        if !text.is_empty() {
                            content.push_str(text);
                            on_text_delta(text);
                        }
                    }
                    if let Some(tcs) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                        for tc in tcs {
                            let idx =
                                tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                            let entry = tool_calls_by_idx.entry(idx).or_default();
                            if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                                if !id.is_empty() {
                                    entry.id = id.to_string();
                                }
                            }
                            if let Some(typ) = tc.get("type").and_then(|v| v.as_str()) {
                                entry.kind = typ.to_string();
                            }
                            if let Some(func) = tc.get("function") {
                                if let Some(name) = func.get("name").and_then(|v| v.as_str()) {
                                    if !name.is_empty() {
                                        entry.name = name.to_string();
                                    }
                                }
                                if let Some(args) = func.get("arguments").and_then(|v| v.as_str()) {
                                    entry.arguments.push_str(args);
                                }
                            }
                        }
                    }
                }
            }
        }

        let tool_calls = if tool_calls_by_idx.is_empty() {
            None
        } else {
            Some(
                tool_calls_by_idx
                    .into_values()
                    .map(|t| crate::chat::ToolCall {
                        id: t.id,
                        kind: if t.kind.is_empty() {
                            "function".into()
                        } else {
                            t.kind
                        },
                        function: crate::chat::FunctionCall {
                            name: t.name,
                            arguments: t.arguments,
                        },
                    })
                    .collect(),
            )
        };

        Ok(ChatRespMessage {
            role: "assistant".into(),
            content: if content.is_empty() {
                None
            } else {
                Some(content)
            },
            tool_calls,
        })
    }
}

#[derive(Default)]
struct AccumTool {
    id: String,
    kind: String,
    name: String,
    arguments: String,
}
