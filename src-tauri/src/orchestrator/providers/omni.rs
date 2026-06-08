//! OmniRouter (OpenAI-compatible) provider.
//!
//! Wraps the existing [`crate::orchestrator::client::OmniClient`] in the
//! [`LlmProvider`] trait so the orchestrator can swap providers via
//! settings.

use super::anthropic::strip_reasoning;
use super::{ChatRequest, ChatRespMessage, DeltaTx, LlmProvider, PingInfo};
use crate::error::{Error, Result};
use crate::orchestrator::client::OmniClient;
use async_trait::async_trait;
use rand::Rng;
use std::time::Duration;

pub struct OmniProvider {
    client: OmniClient,
    label: String,
}

impl OmniProvider {
    pub fn new(base_url: String, model: String, api_key: Option<String>) -> Self {
        Self {
            client: OmniClient::new(base_url, model.clone(), api_key),
            label: "omnirouter".into(),
        }
    }

    /// One streaming attempt against `model` — no retry.
    async fn stream_once(
        &self,
        model: &str,
        req: &ChatRequest,
        delta_tx: &DeltaTx,
    ) -> Result<ChatRespMessage> {
        let tx = delta_tx.clone();
        // `pending` holds raw deltas that may be the start of a reasoning
        // block but don't yet have the closing marker. We forward only
        // confirmed-visible text and emit the buffered tail once we know
        // it's not the opening of a `` / `<|thinking|>` span.
        let pending = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let forward = |d: &str, tx: &DeltaTx, pending: &std::sync::Mutex<String>| {
            // Pull the buffered tail (raw text we've been holding back)
            // plus this new fragment, run the stripper, and split the
            // result into "definitive visible" vs. "still possibly inside
            // a reasoning block". The latter becomes the new buffer.
            let mut buf = pending.lock().expect("reasoning buffer poisoned");
            buf.push_str(d);
            let stripped = strip_reasoning(&buf);
            // Find the longest suffix of `stripped` that *might* still be
            // inside an unclosed `` / `<|thinking|>` block. We keep any
            // tail that ends in the literal `<think>` or `<|thinking|>`
            // token (the marker must land whole — partial UTF-8 is OK
            // because both markers are ASCII).
            let keep = longest_unclosed_reasoning_tail(&stripped);
            if !keep.is_empty() {
                buf.clear();
                buf.push_str(&keep);
                let visible = &stripped[..stripped.len() - keep.len()];
                if !visible.is_empty() {
                    let _ = tx.send(visible.to_string());
                }
            } else {
                buf.clear();
                if !stripped.is_empty() {
                    let _ = tx.send(stripped);
                }
            }
        };
        let p1 = pending.clone();
        let tx1 = tx.clone();
        let resp = if model == self.client.model {
            self.client
                .chat_completions_stream(req.messages.clone(), req.tools.clone(), |d| {
                    forward(&d, &tx1, &p1);
                })
                .await?
        } else {
            let alt = OmniClient::new(
                self.client.base_url.clone(),
                model.to_string(),
                self.client.api_key.clone(),
            );
            let p2 = pending.clone();
            let tx2 = tx.clone();
            alt.chat_completions_stream(req.messages.clone(), req.tools.clone(), |d| {
                forward(&d, &tx2, &p2);
            })
            .await?
        };
        // Flush any trailing reasoning-only buffer (unterminated block —
        // drop it rather than leak it).
        let _ = pending.lock().map(|mut b| b.clear());
        Ok(ChatRespMessage {
            role: resp.role,
            // Defensive: re-strip the assembled text in case reasoning
            // slipped in via a non-streamed code path.
            content: resp.content.as_deref().map(strip_reasoning),
            tool_calls: resp.tool_calls,
        })
    }

    /// Retry wrapper mirroring the Anthropic provider: up to 3 attempts with
    /// jittered exponential backoff on transient errors (5xx / network /
    /// timeout). Non-retryable errors propagate immediately. OmniRouter has
    /// no fallback model, so we just exhaust attempts on the one model.
    async fn stream_with_retry(
        &self,
        req: &ChatRequest,
        delta_tx: &DeltaTx,
    ) -> Result<ChatRespMessage> {
        let mut last_err: Option<Error> = None;
        for attempt in 0..3 {
            match self.stream_once(&req.model, req, delta_tx).await {
                Ok(v) => return Ok(v),
                Err(e) => {
                    if !is_retryable(&e) {
                        return Err(e);
                    }
                    tracing::warn!("omnirouter attempt {} failed: {}", attempt + 1, e);
                    last_err = Some(e);
                    backoff(attempt).await;
                }
            }
        }
        Err(last_err.unwrap_or_else(|| Error::Orchestrator("omnirouter: unknown failure".into())))
    }
}

#[async_trait]
impl LlmProvider for OmniProvider {
    fn label(&self) -> &str {
        &self.label
    }

    fn primary_model(&self) -> &str {
        &self.client.model
    }

    fn fallback_model(&self) -> Option<&str> {
        None
    }

    async fn chat_stream(&self, req: ChatRequest, delta_tx: DeltaTx) -> Result<ChatRespMessage> {
        self.stream_with_retry(&req, &delta_tx).await
    }

    async fn ping(&self) -> Result<PingInfo> {
        // No dedicated health endpoint on OmniRouter. A successful single-shot
        // chat against /v1/chat/completions verifies key + model wiring.
        let messages = vec![serde_json::json!({"role": "user", "content": "ping"})];
        let req = ChatRequest {
            model: self.client.model.clone(),
            messages,
            tools: None,
            temperature: 0.0,
            max_tokens: 8,
            cache: false,
        };
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        match self.chat_stream(req, tx).await {
            Ok(_) => Ok(PingInfo {
                provider: self.label.clone(),
                model: self.client.model.clone(),
                ok: true,
                note: None,
            }),
            Err(e) => Ok(PingInfo {
                provider: self.label.clone(),
                model: self.client.model.clone(),
                ok: false,
                note: Some(e.to_string()),
            }),
        }
    }
}

/// Transient-error classifier for OmniRouter. The `OmniClient` formats its
/// errors as `OmniRouter <status> -> …`, `send: …`, `parse: …`, `stream: …`.
fn is_retryable(e: &Error) -> bool {
    let msg = e.to_string().to_lowercase();
    msg.contains("omnirouter 5")
        || msg.contains("overloaded")
        || msg.contains("timed out")
        || msg.contains("timeout")
        || msg.contains("connection")
        || msg.contains("dns")
        || msg.contains("send:")
        || msg.contains("stream:")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_retryable_classifies_transient_vs_fatal() {
        assert!(is_retryable(&Error::Orchestrator(
            "OmniRouter 503 -> overloaded".into()
        )));
        assert!(is_retryable(&Error::Orchestrator(
            "send: connection reset".into()
        )));
        assert!(is_retryable(&Error::Orchestrator(
            "stream: broken pipe".into()
        )));
        // 4xx and parse errors are NOT retryable.
        assert!(!is_retryable(&Error::Orchestrator(
            "OmniRouter 401 -> bad key".into()
        )));
        assert!(!is_retryable(&Error::Orchestrator(
            "parse: bad json".into()
        )));
    }

    #[test]
    fn unclosed_tail_keeps_think_opener() {
        // "before<think>" — the trailing "<think>" is an unclosed opener;
        // we must hold it back so it can be eaten by the close marker that
        // arrives in the next delta.
        let kept = longest_unclosed_reasoning_tail("before<think>");
        assert_eq!(kept, "<think>");
        // "<think>" alone — keep it all, same reason.
        assert_eq!(longest_unclosed_reasoning_tail("<think>"), "<think>");
        // A complete block has already been stripped by `strip_reasoning`,
        // so the input here is "after"; nothing to keep.
        assert_eq!(longest_unclosed_reasoning_tail("after"), "");
    }

    #[test]
    fn unclosed_tail_handles_qwen_marker() {
        let kept = longest_unclosed_reasoning_tail("x<|thinking|>");
        assert_eq!(kept, "<|thinking|>");
    }
}

/// Return the longest suffix of `s` that *might* still be inside an
/// unclosed `` / `<|thinking|>` reasoning block. The caller is expected
/// to feed this back into [`strip_reasoning`] on the next delta so the
/// reasoning prose never reaches the user.
///
/// We only check for the opening markers (whole token, ASCII). The
/// closing markers are handled inside `strip_reasoning`. If a closing
/// marker arrives in a later delta it will eat the opener and the
/// accumulated tail in one pass.
fn longest_unclosed_reasoning_tail(s: &str) -> String {
    // We look at the LAST occurrence of either opener; everything from
    // there to the end of `s` is "possibly still inside a block".
    let a = s.rfind("<think>");
    let b = s.rfind("<|thinking|>");
    match (a, b) {
        (Some(x), Some(y)) => {
            if x >= y {
                s[x..].to_string()
            } else {
                s[y..].to_string()
            }
        }
        (Some(x), None) => s[x..].to_string(),
        (None, Some(y)) => s[y..].to_string(),
        (None, None) => String::new(),
    }
}
