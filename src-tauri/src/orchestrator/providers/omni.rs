//! OmniRouter (OpenAI-compatible) provider.
//!
//! Wraps the existing [`crate::orchestrator::client::OmniClient`] in the
//! [`LlmProvider`] trait so the orchestrator can swap providers via
//! settings.

use super::{ChatRequest, ChatRespMessage, DeltaTx, LlmProvider, PingInfo};
use crate::error::Result;
use crate::orchestrator::client::OmniClient;
use async_trait::async_trait;

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

    async fn chat_stream(
        &self,
        req: ChatRequest,
        delta_tx: DeltaTx,
    ) -> Result<ChatRespMessage> {
        let resp = if req.model == self.client.model {
            self.client
                .chat_completions_stream(req.messages, req.tools, |d| {
                    let _ = delta_tx.send(d.to_string());
                })
                .await?
        } else {
            let alt = OmniClient::new(
                self.client.base_url.clone(),
                req.model.clone(),
                self.client.api_key.clone(),
            );
            alt.chat_completions_stream(req.messages, req.tools, |d| {
                let _ = delta_tx.send(d.to_string());
            })
            .await?
        };
        Ok(ChatRespMessage {
            role: resp.role,
            content: resp.content,
            tool_calls: resp.tool_calls,
        })
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
