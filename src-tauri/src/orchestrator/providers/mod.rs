//! LLM provider abstraction.
//!
//! The Orchestrator drives chat through a [`LlmProvider`] trait so different
//! backends (Anthropic Messages API, OmniRouter / OpenAI-compatible) can be
//! selected at runtime via the `provider` setting.
//!
//! New providers add a module here and a branch in [`build_provider`].

pub mod anthropic;
pub mod omni;

use crate::chat::ToolCall;
use crate::db::{self, DbPool};
use crate::error::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

/// Provider-neutral chat request.
#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    /// OpenAI-shape messages (system/user/assistant/tool). Anthropic adapter
    /// translates internally.
    pub messages: Vec<Value>,
    /// OpenAI-shape tools. Adapter converts to provider schema.
    pub tools: Option<Vec<Value>>,
    pub temperature: f32,
    pub max_tokens: u32,
    /// If `true`, the adapter is allowed to attach `cache_control` markers to
    /// stable regions (system + tools). Honoured only by providers that
    /// support it.
    pub cache: bool,
}

/// Provider-neutral assistant response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRespMessage {
    pub role: String,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCall>>,
}

/// Healthcheck payload returned by `ping`.
#[derive(Debug, Clone, Serialize)]
pub struct PingInfo {
    pub provider: String,
    pub model: String,
    pub ok: bool,
    pub note: Option<String>,
}

/// Streaming text-delta channel. The trait takes an `mpsc::Sender` instead
/// of a callback so the future stays `Send + 'static` under async_trait,
/// without entangling lifetimes of caller-owned closures.
pub type DeltaTx = tokio::sync::mpsc::UnboundedSender<String>;

/// Common provider interface.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn label(&self) -> &str;
    fn primary_model(&self) -> &str;
    fn fallback_model(&self) -> Option<&str>;

    async fn chat_stream(
        &self,
        req: ChatRequest,
        delta_tx: DeltaTx,
    ) -> Result<ChatRespMessage>;

    async fn ping(&self) -> Result<PingInfo>;
}

// ---------------- Settings keys ----------------

pub const KEY_PROVIDER: &str = "provider";
pub const KEY_ARCH_MODEL: &str = "architect.model";
pub const KEY_ARCH_FALLBACK: &str = "architect.fallback_model";
pub const KEY_ARCH_FALLBACK_ON: &str = "architect.fallback_enabled";
pub const KEY_ARCH_CACHE_ON: &str = "architect.cache_enabled";
pub const KEY_ARCH_MAX_TOKENS: &str = "architect.max_tokens";

pub const KEY_ANTHROPIC_API_KEY: &str = "anthropic.api_key";
pub const KEY_ANTHROPIC_BASE: &str = "anthropic.base_url";

pub const KEY_OMNI_BASE: &str = "omnirouter.base_url";
pub const KEY_OMNI_MODEL: &str = "omnirouter.model";
pub const KEY_OMNI_API_KEY: &str = "omnirouter.api_key";

pub const DEFAULT_ARCH_MODEL: &str = "claude-opus-4-5";
pub const DEFAULT_ARCH_FALLBACK: &str = "claude-opus-4";
pub const DEFAULT_ANTHROPIC_BASE: &str = "https://api.anthropic.com";
pub const DEFAULT_OMNI_BASE: &str = "http://localhost:20128";
pub const DEFAULT_OMNI_MODEL: &str = "kr/claude-opus-4.7";

/// Resolve the Anthropic API key in priority order: env > setting.
pub fn resolve_anthropic_key(db: &DbPool) -> Option<String> {
    if let Ok(v) = std::env::var("ANTHROPIC_API_KEY") {
        if !v.trim().is_empty() {
            return Some(v);
        }
    }
    db::get_setting(db, KEY_ANTHROPIC_API_KEY)
        .ok()
        .flatten()
        .filter(|s| !s.trim().is_empty())
}

/// Build the active provider. Hardcoded to OmniRouter with kr/claude-opus-4.6.
pub fn build_provider(_db: &DbPool) -> Arc<dyn LlmProvider> {
    Arc::new(omni::OmniProvider::new(
        DEFAULT_OMNI_BASE.to_string(),
        DEFAULT_OMNI_MODEL.to_string(),
        None,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn temp_pool() -> DbPool {
        let manager = r2d2_sqlite::SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(2).build(manager).unwrap();
        let conn = pool.get().unwrap();
        conn.execute(
            "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
            [],
        )
        .unwrap();
        pool
    }

    #[test]
    fn build_provider_uses_omnirouter_kr() {
        let db = temp_pool();
        let p = build_provider(&db);
        assert_eq!(p.label(), "omnirouter");
        assert_eq!(p.primary_model(), DEFAULT_OMNI_MODEL);
    }

    #[test]
    fn provider_ignores_settings() {
        let db = temp_pool();
        db::set_setting(&db, KEY_PROVIDER, "anthropic").unwrap();
        db::set_setting(&db, KEY_ARCH_MODEL, "claude-opus-4-5").unwrap();
        let p = build_provider(&db);
        assert_eq!(p.primary_model(), DEFAULT_OMNI_MODEL);
    }
}
