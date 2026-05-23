//! Cloud streaming engine — Deepgram Nova-3 (opt-in).
//!
//! This is the v1 wiring: settings storage for the API key + endpoint, a
//! tiny config struct, and a synchronous "is configured?" helper used by
//! [`crate::voice::VoicePipeline`] to decide whether to attempt the cloud
//! path. Full WebSocket streaming is gated behind `voice.engine = deepgram`
//! and is implemented in a follow-up patch — see [PIGVOICE_PLAN.md].
//!
//! What lives here:
//!
//! * key/endpoint persistence in the settings KV (never written to git);
//! * a [`CloudConfig`] aggregate consumed by the cloud client;
//! * runtime helpers (`is_configured`, `language_or_default`) that the
//!   engine selector calls before deciding to fall back to on-device.
//!
//! What does NOT live here:
//!
//! * the WebSocket client itself — it depends on `tokio-tungstenite`,
//!   which is not yet in the workspace `Cargo.toml`. Adding it costs us
//!   nothing on-device, but we want to keep the v1 PR scope tight: the
//!   on-device streaming engine ships first; cloud follows.

use crate::db::{self, DbPool};
use crate::error::Result;

/// Settings keys.
pub const SETTING_KEY: &str = "voice.cloud_api_key";
pub const SETTING_ENDPOINT: &str = "voice.cloud_endpoint";
pub const SETTING_LANGUAGE: &str = "voice.cloud_language";

/// Default Deepgram Nova-3 streaming endpoint.
pub const DEFAULT_ENDPOINT: &str =
    "wss://api.deepgram.com/v1/listen?model=nova-3&interim_results=true&endpointing=400";

/// All settings the cloud engine needs to make a connection.
#[derive(Debug, Clone)]
pub struct CloudConfig {
    pub api_key: String,
    pub endpoint: String,
    pub language: Option<String>,
}

impl CloudConfig {
    /// Read the cloud config from settings. Returns `None` if no API key
    /// is set — the engine selector treats `None` as "not configured" and
    /// falls back to on-device.
    pub fn load(db: &DbPool) -> Result<Option<Self>> {
        let key = match db::get_setting(db, SETTING_KEY)? {
            Some(k) if !k.trim().is_empty() => k,
            _ => return Ok(None),
        };
        let endpoint = db::get_setting(db, SETTING_ENDPOINT)?
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_ENDPOINT.to_string());
        let language =
            db::get_setting(db, SETTING_LANGUAGE)?.filter(|s| !s.trim().is_empty() && s != "auto");
        Ok(Some(Self {
            api_key: key,
            endpoint,
            language,
        }))
    }

    /// Whether a key is present (no network probe — purely a settings check).
    pub fn is_configured(db: &DbPool) -> bool {
        Self::load(db).ok().flatten().is_some()
    }

    /// Persist the API key (creating or overwriting). Empty string clears.
    pub fn store_key(db: &DbPool, key: &str) -> Result<()> {
        db::set_setting(db, SETTING_KEY, key.trim())
    }

    /// Persist the endpoint. Empty string resets to default.
    pub fn store_endpoint(db: &DbPool, endpoint: &str) -> Result<()> {
        db::set_setting(db, SETTING_ENDPOINT, endpoint.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool() -> DbPool {
        let manager = r2d2_sqlite::SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(2).build(manager).unwrap();
        pool.get()
            .unwrap()
            .execute_batch("CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);")
            .unwrap();
        pool
    }

    #[test]
    fn is_configured_false_by_default() {
        let p = pool();
        assert!(!CloudConfig::is_configured(&p));
        assert!(CloudConfig::load(&p).unwrap().is_none());
    }

    #[test]
    fn store_key_and_load_round_trips() {
        let p = pool();
        CloudConfig::store_key(&p, "abc123").unwrap();
        let cfg = CloudConfig::load(&p).unwrap().expect("configured");
        assert_eq!(cfg.api_key, "abc123");
        assert_eq!(cfg.endpoint, DEFAULT_ENDPOINT);
        assert!(cfg.language.is_none());
    }

    #[test]
    fn empty_key_means_not_configured() {
        let p = pool();
        CloudConfig::store_key(&p, "   ").unwrap();
        assert!(!CloudConfig::is_configured(&p));
    }

    #[test]
    fn custom_endpoint_takes_precedence() {
        let p = pool();
        CloudConfig::store_key(&p, "k").unwrap();
        CloudConfig::store_endpoint(&p, "wss://example.com/v1/listen").unwrap();
        let cfg = CloudConfig::load(&p).unwrap().unwrap();
        assert_eq!(cfg.endpoint, "wss://example.com/v1/listen");
    }
}
