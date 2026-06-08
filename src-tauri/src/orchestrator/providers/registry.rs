//! Custom-provider registry: CRUD, model auto-discovery, and the factory
//! that turns a stored provider row into a live [`LlmProvider`].
//!
//! Two endpoint formats are supported:
//! - **OpenAI-compatible** — `GET {base}/v1/models` with `Authorization:
//!   Bearer <key>`; models live under `data[].id`. Driven at chat time by
//!   [`super::omni::OmniProvider`].
//! - **Anthropic** — `GET {base}/v1/models` with `x-api-key` +
//!   `anthropic-version`; models live under `data[].id`. Driven at chat time
//!   by [`super::anthropic::AnthropicProvider`].

use crate::db::{self, CustomProviderRow, DbPool};
use crate::error::{Error, Result};
use crate::secrets;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use uuid::Uuid;

use super::{
    anthropic::AnthropicProvider, omni::OmniProvider, LlmProvider, DEFAULT_ANTHROPIC_BASE,
    DEFAULT_OPENAI_BASE,
};

/// Anthropic API version used for both `/v1/models` and `/v1/messages`.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Setting key holding the id of the active custom provider. When set + the
/// row exists + has a model, the gateway routes through it.
pub const KEY_ACTIVE_PROVIDER: &str = "provider.active_id";

/// Event fired whenever the active provider pointer (or its row) changes —
/// the UI subscribes to refresh the "current model" badge in the chat header.
pub const EV_PROVIDER_CHANGED: &str = "provider://changed";

/// Notify the UI that the active provider may have changed. Safe to call from
/// any Tauri command that received an `AppHandle`; a no-op if the channel is
/// closed (e.g. window torn down mid-write).
pub fn emit_provider_changed(app: &AppHandle) {
    let _ = app.emit(EV_PROVIDER_CHANGED, ());
}

/// Provider format. Mirrors the `kind` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    OpenAi,
    Anthropic,
}

impl ProviderKind {
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "openai" | "openai-compatible" | "openai_compatible" => Ok(Self::OpenAi),
            "anthropic" => Ok(Self::Anthropic),
            other => Err(Error::Invalid(format!(
                "unknown provider kind '{}' (expected 'openai' or 'anthropic')",
                other
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
        }
    }

    /// Official default endpoint for this format. Used when the user leaves
    /// the Base URL blank — OpenAI and Anthropic both have a well-known host.
    pub fn default_base_url(self) -> &'static str {
        match self {
            Self::OpenAi => DEFAULT_OPENAI_BASE,
            Self::Anthropic => DEFAULT_ANTHROPIC_BASE,
        }
    }
}

/// Public view of a provider for the UI. Never carries the API key — only
/// whether one is stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderView {
    pub id: String,
    pub label: String,
    pub kind: String,
    pub base_url: String,
    pub model: String,
    pub created_at: String,
    pub has_api_key: bool,
    pub is_active: bool,
}

fn to_view(pool: &DbPool, row: CustomProviderRow, active_id: &str) -> ProviderView {
    let has_api_key =
        secrets::has_secret(pool, &secrets::provider_key_name(&row.id)).unwrap_or(false);
    ProviderView {
        is_active: row.id == active_id,
        has_api_key,
        id: row.id,
        label: row.label,
        kind: row.kind,
        base_url: row.base_url,
        model: row.model,
        created_at: row.created_at,
    }
}

/// Resolve + validate a base URL for the given format. An empty input falls
/// back to the format's official default endpoint (OpenAI/Anthropic both have
/// a well-known host), so the user only needs to fill it in for self-hosted
/// or third-party gateways.
///
/// The gateway and discovery layers append a path (`/v1/models`,
/// `/v1/chat/completions`, `/v1/messages`) to whatever is stored here, so the
/// base must NOT itself end in an API path. We forgive the two most common
/// paste mistakes (`.../v1` and a full `.../v1/<endpoint>`) by stripping them,
/// and reject query/fragment which would corrupt the concatenated URL.
fn normalize_base_url(kind: ProviderKind, raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(kind.default_base_url().to_string());
    }
    let parsed =
        url::Url::parse(trimmed).map_err(|e| Error::Invalid(format!("invalid base URL: {}", e)))?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => {
            return Err(Error::Invalid(format!(
                "base URL scheme must be http(s), got '{}'",
                other
            )))
        }
    }
    if parsed.host_str().map(|h| h.is_empty()).unwrap_or(true) {
        return Err(Error::Invalid("base URL has no host".into()));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(Error::Invalid(
            "base URL must not contain a query string or fragment".into(),
        ));
    }
    // Strip trailing slash, then a trailing API path the user may have pasted
    // (`/v1/models`, `/v1/messages`, `/v1/chat/completions`, or a bare `/v1`).
    let mut s = trimmed.trim_end_matches('/').to_string();
    for suffix in ["/v1/chat/completions", "/v1/messages", "/v1/models", "/v1"] {
        if let Some(stripped) = s.strip_suffix(suffix) {
            s = stripped.trim_end_matches('/').to_string();
            break;
        }
    }
    if s.is_empty() {
        return Err(Error::Invalid("base URL is missing a host".into()));
    }
    Ok(s)
}

// ----------------------------- CRUD -----------------------------

pub fn list(pool: &DbPool) -> Result<Vec<ProviderView>> {
    let active = active_id(pool);
    let rows = db::list_custom_providers(pool)?;
    Ok(rows
        .into_iter()
        .map(|r| to_view(pool, r, &active))
        .collect())
}

pub fn get(pool: &DbPool, id: &str) -> Result<ProviderView> {
    let row = db::get_custom_provider(pool, id)?
        .ok_or_else(|| Error::NotFound(format!("provider '{}'", id)))?;
    Ok(to_view(pool, row, &active_id(pool)))
}

/// Create a provider. If `api_key` is non-empty it is encrypted into the
/// secret store; the row itself never holds the key.
pub fn create(
    pool: &DbPool,
    label: &str,
    kind: &str,
    base_url: &str,
    api_key: Option<&str>,
) -> Result<ProviderView> {
    let view = create_inner(pool, label, kind, base_url, api_key)?;
    Ok(view)
}

/// Internal builder — separate from the public `create` so callers that
/// hold an `AppHandle` can emit the change event after success.
fn create_inner(
    pool: &DbPool,
    label: &str,
    kind: &str,
    base_url: &str,
    api_key: Option<&str>,
) -> Result<ProviderView> {
    let kind = ProviderKind::parse(kind)?;
    let base = normalize_base_url(kind, base_url)?;
    let label = label.trim();
    if label.is_empty() {
        return Err(Error::Invalid("label is required".into()));
    }
    let id = Uuid::new_v4().to_string();
    // Store the key FIRST. If encryption/secret-store fails we abort before
    // creating the row, so we never leave a keyless orphan provider behind.
    if let Some(key) = api_key {
        if !key.trim().is_empty() {
            secrets::set_secret(pool, &secrets::provider_key_name(&id), key.trim())?;
        }
    }
    let row = CustomProviderRow {
        id: id.clone(),
        label: label.to_string(),
        kind: kind.as_str().to_string(),
        base_url: base,
        model: String::new(),
        created_at: Utc::now().to_rfc3339(),
    };
    if let Err(e) = db::insert_custom_provider(pool, &row) {
        // Roll back the secret we just wrote so it doesn't dangle.
        let _ = secrets::delete_secret(pool, &secrets::provider_key_name(&id));
        return Err(e);
    }
    get(pool, &id)
}

/// Update mutable fields. `api_key=Some("")` is treated as "leave key
/// untouched"; pass a non-empty string to rotate it.
pub fn update(
    pool: &DbPool,
    id: &str,
    label: &str,
    base_url: &str,
    model: &str,
    api_key: Option<&str>,
) -> Result<ProviderView> {
    let view = update_inner(pool, id, label, base_url, model, api_key)?;
    Ok(view)
}

/// Internal builder — separate so the AppHandle-aware command wrapper can
/// fire the change event after success.
fn update_inner(
    pool: &DbPool,
    id: &str,
    label: &str,
    base_url: &str,
    model: &str,
    api_key: Option<&str>,
) -> Result<ProviderView> {
    let existing = db::get_custom_provider(pool, id)?
        .ok_or_else(|| Error::NotFound(format!("provider '{}'", id)))?;
    let label = label.trim();
    if label.is_empty() {
        return Err(Error::Invalid("label is required".into()));
    }
    let kind = ProviderKind::parse(&existing.kind)?;
    let base = normalize_base_url(kind, base_url)?;
    db::update_custom_provider(pool, id, label, &base, model.trim())?;
    if let Some(key) = api_key {
        if !key.trim().is_empty() {
            secrets::set_secret(pool, &secrets::provider_key_name(id), key.trim())?;
        }
    }
    let _ = existing;
    get(pool, id)
}

pub fn delete(pool: &DbPool, id: &str) -> Result<()> {
    db::delete_custom_provider(pool, id)?;
    let _ = secrets::delete_secret(pool, &secrets::provider_key_name(id));
    // If the active provider was just deleted, clear the pointer so the
    // gateway falls back to the built-in default.
    if active_id(pool) == id {
        let _ = db::set_setting(pool, KEY_ACTIVE_PROVIDER, "");
    }
    Ok(())
}

/// Persist the user's model choice for a provider.
pub fn set_model(pool: &DbPool, id: &str, model: &str) -> Result<ProviderView> {
    set_model_inner(pool, id, model)
}

fn set_model_inner(pool: &DbPool, id: &str, model: &str) -> Result<ProviderView> {
    db::get_custom_provider(pool, id)?
        .ok_or_else(|| Error::NotFound(format!("provider '{}'", id)))?;
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return Err(Error::Invalid("model is required".into()));
    }
    db::set_custom_provider_model(pool, id, trimmed)?;
    get(pool, id)
}

// --------------------------- Active pointer ---------------------------

pub fn active_id(pool: &DbPool) -> String {
    db::get_setting(pool, KEY_ACTIVE_PROVIDER)
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// Mark a provider active. Empty string clears it (back to built-in default).
pub fn set_active(pool: &DbPool, id: &str) -> Result<()> {
    set_active_inner(pool, id)
}

fn set_active_inner(pool: &DbPool, id: &str) -> Result<()> {
    if id.is_empty() {
        return db::set_setting(pool, KEY_ACTIVE_PROVIDER, "");
    }
    db::get_custom_provider(pool, id)?
        .ok_or_else(|| Error::NotFound(format!("provider '{}'", id)))?;
    db::set_setting(pool, KEY_ACTIVE_PROVIDER, id)
}

// --------------------------- Model discovery ---------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ModelEntry {
    pub id: String,
}

fn discovery_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .expect("reqwest client")
}

/// Map low-level fetch failures to a short, user-facing message. The provider
/// key is never included.
fn friendly_http_error(status: reqwest::StatusCode, body: &str) -> String {
    let snippet = body.chars().take(180).collect::<String>();
    match status.as_u16() {
        401 | 403 => "authentication failed — check the API key".into(),
        404 => "endpoint not found — check the base URL (expected <base>/v1/models)".into(),
        429 => "rate limited by the provider — try again shortly".into(),
        500..=599 => format!("provider error ({})", status),
        _ => format!("HTTP {} — {}", status, snippet),
    }
}

/// Fetch the available model ids for a stored provider, using its saved key.
pub async fn fetch_models_for(pool: &DbPool, id: &str) -> Result<Vec<ModelEntry>> {
    let row = db::get_custom_provider(pool, id)?
        .ok_or_else(|| Error::NotFound(format!("provider '{}'", id)))?;
    let key = secrets::get_secret(pool, &secrets::provider_key_name(id))?;
    let kind = ProviderKind::parse(&row.kind)?;
    fetch_models(kind, &row.base_url, key.as_deref()).await
}

/// Fetch models against an arbitrary endpoint — used by the "test before
/// save" flow where the provider is not persisted yet.
pub async fn fetch_models(
    kind: ProviderKind,
    base_url: &str,
    api_key: Option<&str>,
) -> Result<Vec<ModelEntry>> {
    let base = normalize_base_url(kind, base_url)?;
    let url = format!("{}/v1/models", base);
    let client = discovery_client();

    let req = match kind {
        ProviderKind::OpenAi => {
            let mut r = client.get(&url);
            if let Some(k) = api_key.filter(|k| !k.trim().is_empty()) {
                r = r.bearer_auth(k.trim());
            }
            r
        }
        ProviderKind::Anthropic => {
            let key = api_key
                .filter(|k| !k.trim().is_empty())
                .ok_or_else(|| Error::Invalid("Anthropic requires an API key".into()))?;
            client
                .get(&url)
                .header("x-api-key", key.trim())
                .header("anthropic-version", ANTHROPIC_VERSION)
        }
    };

    let resp = req.send().await.map_err(|e| {
        if e.is_timeout() {
            Error::Invalid("request timed out — is the endpoint reachable?".into())
        } else if e.is_connect() {
            Error::Invalid("could not connect — check the base URL".into())
        } else {
            Error::Invalid(format!("request failed: {}", e))
        }
    })?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        // Never log the body at error level here — keep discovery failures
        // quiet and return a friendly message for the UI.
        return Err(Error::Invalid(friendly_http_error(status, &body)));
    }

    let json: Value = resp
        .json()
        .await
        .map_err(|e| Error::Invalid(format!("could not parse model list: {}", e)))?;
    let models = parse_models(&json);
    if models.is_empty() {
        return Err(Error::Invalid(
            "endpoint returned no models (unexpected response shape)".into(),
        ));
    }
    Ok(models)
}

/// Extract model ids from the common response shapes:
/// - OpenAI / Anthropic: `{ "data": [ { "id": "..." }, ... ] }`
/// - bare array: `[ { "id": "..." }, ... ]` or `[ "id", ... ]`
fn parse_models(json: &Value) -> Vec<ModelEntry> {
    let arr = json
        .get("data")
        .and_then(|d| d.as_array())
        .or_else(|| json.as_array());
    let Some(arr) = arr else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in arr {
        let id = if let Some(s) = item.as_str() {
            Some(s.to_string())
        } else {
            item.get("id")
                .and_then(|v| v.as_str())
                .or_else(|| item.get("name").and_then(|v| v.as_str()))
                .map(|s| s.to_string())
        };
        if let Some(id) = id {
            if !id.is_empty() {
                out.push(ModelEntry { id });
            }
        }
    }
    out
}

// --------------------------- Gateway factory ---------------------------

/// Build a live provider from a stored row + its saved key. Used by the
/// gateway when a custom provider is active.
pub fn build_from_row(pool: &DbPool, row: &CustomProviderRow) -> Result<Arc<dyn LlmProvider>> {
    let kind = ProviderKind::parse(&row.kind)?;
    let key = secrets::get_secret(pool, &secrets::provider_key_name(&row.id))?;
    if row.model.trim().is_empty() {
        return Err(Error::Invalid(format!(
            "provider '{}' has no model selected",
            row.label
        )));
    }
    // Defensive: a row persisted before base-URL normalization (or hand-edited)
    // could carry an empty base. Fall back to the format's official endpoint
    // rather than building a provider that points at "/v1/...".
    let base = if row.base_url.trim().is_empty() {
        kind.default_base_url().to_string()
    } else {
        row.base_url.clone()
    };
    Ok(match kind {
        ProviderKind::OpenAi => Arc::new(OmniProvider::new(base, row.model.clone(), key)),
        ProviderKind::Anthropic => Arc::new(AnthropicProvider::new(
            base,
            row.model.clone(),
            None,
            true,
            8192,
            key,
        )),
    })
}

/// Resolve the active custom provider into a live [`LlmProvider`], if one is
/// configured and usable. Returns `None` to signal "use the built-in
/// default".
pub fn active_provider(pool: &DbPool) -> Option<Arc<dyn LlmProvider>> {
    let id = active_id(pool);
    if id.is_empty() {
        return None;
    }
    let row = db::get_custom_provider(pool, &id).ok().flatten()?;
    match build_from_row(pool, &row) {
        Ok(p) => Some(p),
        Err(e) => {
            tracing::warn!("active custom provider '{}' unusable: {}", row.label, e);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_openai_data_shape() {
        let j = json!({"data": [{"id": "gpt-4o"}, {"id": "gpt-4o-mini"}]});
        let m = parse_models(&j);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].id, "gpt-4o");
    }

    #[test]
    fn parse_anthropic_data_shape() {
        let j = json!({"data": [{"id": "claude-opus-4-20250101", "display_name": "Opus"}]});
        let m = parse_models(&j);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].id, "claude-opus-4-20250101");
    }

    #[test]
    fn parse_bare_array_and_strings() {
        let j = json!(["a", {"id": "b"}, {"name": "c"}]);
        let m = parse_models(&j);
        let ids: Vec<_> = m.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    #[test]
    fn parse_empty_when_unexpected() {
        assert!(parse_models(&json!({"foo": 1})).is_empty());
    }

    #[test]
    fn kind_parse_aliases() {
        assert_eq!(ProviderKind::parse("OpenAI").unwrap(), ProviderKind::OpenAi);
        assert_eq!(
            ProviderKind::parse("anthropic").unwrap(),
            ProviderKind::Anthropic
        );
        assert!(ProviderKind::parse("gemini").is_err());
    }

    #[test]
    fn base_url_validation() {
        // Empty input resolves to the format's official default endpoint.
        assert_eq!(
            normalize_base_url(ProviderKind::OpenAi, "").unwrap(),
            DEFAULT_OPENAI_BASE
        );
        assert_eq!(
            normalize_base_url(ProviderKind::Anthropic, "  ").unwrap(),
            DEFAULT_ANTHROPIC_BASE
        );
        // Non-http(s) scheme rejected.
        assert!(normalize_base_url(ProviderKind::OpenAi, "ftp://x").is_err());
        // Garbage / no host rejected.
        assert!(normalize_base_url(ProviderKind::OpenAi, "not a url").is_err());
        assert!(normalize_base_url(ProviderKind::OpenAi, "https://").is_err());
        // Trailing slash trimmed.
        assert_eq!(
            normalize_base_url(ProviderKind::OpenAi, "https://api.x.com/").unwrap(),
            "https://api.x.com"
        );
    }

    #[test]
    fn base_url_strips_pasted_api_paths() {
        // Users often paste the full endpoint; strip the known API suffixes so
        // we don't end up requesting `/v1/models/v1/models`.
        for (input, expected) in [
            ("https://api.openai.com/v1", "https://api.openai.com"),
            ("https://api.openai.com/v1/", "https://api.openai.com"),
            ("https://api.openai.com/v1/models", "https://api.openai.com"),
            (
                "https://api.openai.com/v1/chat/completions",
                "https://api.openai.com",
            ),
            (
                "https://api.anthropic.com/v1/messages",
                "https://api.anthropic.com",
            ),
            // A real base path that merely contains "v1" elsewhere is kept.
            (
                "https://gw.example.com/openai",
                "https://gw.example.com/openai",
            ),
        ] {
            assert_eq!(
                normalize_base_url(ProviderKind::OpenAi, input).unwrap(),
                expected,
                "input={input}"
            );
        }
    }

    #[test]
    fn base_url_rejects_query_and_fragment() {
        assert!(normalize_base_url(ProviderKind::OpenAi, "https://api.x.com/?k=1").is_err());
        assert!(normalize_base_url(ProviderKind::OpenAi, "https://api.x.com/#frag").is_err());
    }

    /// End-to-end: prove that an arbitrary model name (typed by the user,
    /// never returned by `/v1/models`) is accepted and used by the gateway.
    /// This is the heart of the "any model" guarantee — no model allow-list,
    /// no validation against a fetched list. The string is sent verbatim.
    #[test]
    fn any_model_string_is_accepted_and_routed() {
        use super::super::build_provider;
        use crate::db;
        let manager = r2d2_sqlite::SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(2).build(manager).unwrap();
        let conn = pool.get().unwrap();
        conn.execute(
            "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
            [],
        )
        .unwrap();
        conn.execute(
            "CREATE TABLE custom_providers (
                id TEXT PRIMARY KEY,
                label TEXT NOT NULL,
                kind TEXT NOT NULL,
                base_url TEXT NOT NULL,
                model TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL
             )",
            [],
        )
        .unwrap();
        // Pre-create the secrets table for key persistence.
        conn.execute(
            "CREATE TABLE secrets (name TEXT PRIMARY KEY, value TEXT NOT NULL)",
            [],
        )
        .unwrap();

        // Realistic "wild" inputs: third-party gateway with vendor-prefixed
        // model name, dated snapshot, hyphenated slug, versioned tag. None of
        // these would survive an allow-list — they must all round-trip.
        for (label, base, kind, model, expected_label) in [
            (
                "OpenRouter",
                "https://openrouter.ai/api",
                "openai",
                "anthropic/claude-3.5-sonnet",
                "omnirouter",
            ),
            (
                "Local llama",
                "http://localhost:11434/v1",
                "openai",
                "llama3.1:70b-instruct-q4_K_M",
                "omnirouter",
            ),
            (
                "Azure OpenAI",
                "https://my-org.openai.azure.com/openai/deployments/gpt4o",
                "openai",
                "gpt-4o-2024-08-06",
                "omnirouter",
            ),
            (
                "Custom gateway",
                "https://gw.internal.example.com",
                "openai",
                "my-team-finetune-v3.2",
                "omnirouter",
            ),
            (
                "Vertex Anthropic",
                "https://us-central1-aiplatform.googleapis.com",
                "anthropic",
                "claude-opus-4@20250101",
                "anthropic",
            ),
        ] {
            let v = create(&pool, label, kind, base, Some("sk-test")).unwrap();
            assert!(v.has_api_key, "{label}: key should be persisted");
            set_model(&pool, &v.id, model).unwrap();
            set_active(&pool, &v.id).unwrap();
            let p = build_provider(&pool);
            assert_eq!(
                p.primary_model(),
                model,
                "{label}: gateway sent the wrong model"
            );
            assert_eq!(p.label(), expected_label, "{label}: wrong adapter");
            // Tear down for the next iteration.
            delete(&pool, &v.id).unwrap();
        }
    }

    /// The model string is stored verbatim — no validation, no normalization
    /// beyond `.trim()`. Empty / whitespace-only inputs are rejected so the
    /// gateway never ships a `model=""` request.
    #[test]
    fn set_model_rejects_empty_after_trim() {
        use super::super::build_provider;
        use crate::db;
        let manager = r2d2_sqlite::SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(2).build(manager).unwrap();
        let conn = pool.get().unwrap();
        conn.execute_batch(
            "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE custom_providers (
                 id TEXT PRIMARY KEY, label TEXT NOT NULL, kind TEXT NOT NULL,
                 base_url TEXT NOT NULL, model TEXT NOT NULL DEFAULT '',
                 created_at TEXT NOT NULL
             );
             CREATE TABLE secrets (name TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )
        .unwrap();

        let v = create(&pool, "L", "openai", "https://api.openai.com", None).unwrap();
        for bad in ["", "   ", "\t\n"] {
            let err = set_model(&pool, &v.id, bad).unwrap_err();
            assert!(
                matches!(err, Error::Invalid(_)),
                "expected Invalid for {bad:?}"
            );
        }
        // But surrounding whitespace is fine — the gateway gets a clean string.
        set_model(&pool, &v.id, "  gpt-4o  ").unwrap();
        set_active(&pool, &v.id).unwrap();
        let p = build_provider(&pool);
        assert_eq!(p.primary_model(), "gpt-4o");
    }
}
