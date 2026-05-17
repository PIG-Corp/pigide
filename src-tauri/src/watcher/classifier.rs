//! Gemini classifier for agent stdout chunks.
//!
//! Calls `gemma-3-4b-it` via Google AI Studio's Generative Language API
//! (`v1beta/models/gemma-3-4b-it:generateContent`) with a strict-JSON prompt
//! and parses the response into a [`Classification`].
//!
//! Secrets handling:
//! * `GEMINI_API_KEY` is read from the environment exactly once per call —
//!   never logged, never returned in error strings.
//! * Errors carry only the HTTP status / a stripped message; a dedicated
//!   helper [`redact_secret`] is used by tests to confirm the key never
//!   leaks into our `Display` output.

use serde::{Deserialize, Serialize};

/// Classification verdict produced by Gemini for a single stdout chunk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Classification {
    /// `decision_request` when the agent has paused on a question / choice.
    /// `noise` for everything else (progress logs, banners, ANSI repaints).
    pub kind: ClassifierKind,
    /// Cleaned-up question the agent is asking. Only populated for
    /// `decision_request`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_text: Option<String>,
    /// Discrete options surfaced by the agent (e.g. ["yes", "no", "abort"]).
    /// Empty vec when not applicable.
    #[serde(default)]
    pub options: Vec<String>,
}

/// Two-state classification — anything that is not a decision request is
/// noise the supervisor can ignore.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassifierKind {
    DecisionRequest,
    Noise,
}

impl ClassifierKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ClassifierKind::DecisionRequest => "decision_request",
            ClassifierKind::Noise => "noise",
        }
    }
}

/// System prompt sent to Gemini before every chunk. Kept minimal — Gemma
/// follows JSON-shape instructions much better when they fit on a screen.
pub const SYSTEM_PROMPT: &str = "\
You classify a single chunk of stdout from an autonomous CLI coding agent. \
Decide whether the agent is asking a human for a decision (yes/no, pick an \
option, supply a value) or just printing progress / errors / noise.

Respond with STRICT JSON, no markdown fences, no commentary. Schema:
{\"kind\": \"decision_request\"|\"noise\", \"prompt_text\": string|null, \
\"options\": [string]}

Rules:
- \"decision_request\" only when the very last visible line is a prompt the \
  agent is BLOCKED on (e.g. '(y/N)', numbered choice, '> ', 'Enter password').
- Plain progress (\"compiling…\", \"running tests\"), errors without a \
  prompt, ANSI repaints, banners → \"noise\".
- prompt_text: the question reduced to one short sentence, or null for noise.
- options: discrete choices the agent surfaced (in order); [] if free-form \
  or noise.";

/// Endpoint for `gemma-3-4b-it`. Overridable via [`GeminiClient::with_endpoint`]
/// for tests.
pub const GEMINI_ENDPOINT: &str =
    "https://generativelanguage.googleapis.com/v1beta/models/gemma-3-4b-it:generateContent";

/// Strip a candidate `?key=...` query param from any string. Used in error
/// paths so a panic / log call cannot accidentally surface the API key.
pub fn redact_secret(input: &str, secret: &str) -> String {
    if secret.is_empty() {
        return input.to_string();
    }
    input.replace(secret, "[REDACTED]")
}

/// Parse Gemini's `candidates[0].content.parts[0].text` payload — which we
/// asked to be strict JSON — into a [`Classification`].
///
/// Tolerant to:
/// * a leading/trailing markdown fence (```json … ```), defensively stripped;
/// * extra whitespace;
/// * missing `options` field (treated as empty);
/// * `prompt_text: ""` (treated as `None`).
///
/// Returns `Err` on any other malformed payload — caller treats that as
/// "drop this chunk, it's noise".
pub fn parse_classification(raw: &str) -> Result<Classification, String> {
    let cleaned = strip_json_fence(raw.trim());
    let mut value: Classification = serde_json::from_str(cleaned)
        .map_err(|e| format!("classifier JSON parse: {}", e))?;
    // Normalize empty strings -> None to keep downstream code simple.
    if let Some(s) = value.prompt_text.as_ref() {
        if s.trim().is_empty() {
            value.prompt_text = None;
        }
    }
    Ok(value)
}

fn strip_json_fence(s: &str) -> &str {
    // Conservative: only peel ```json … ``` or ``` … ``` if both ends match.
    let s = s.trim();
    let inner = s
        .strip_prefix("```json")
        .or_else(|| s.strip_prefix("```"))
        .map(|x| x.trim_start_matches('\n'))
        .unwrap_or(s);
    inner.strip_suffix("```").map(|x| x.trim()).unwrap_or(inner)
}

/// Thin reqwest-based client. Holds the endpoint and the API key so call
/// sites don't have to thread `std::env::var` everywhere — and so tests can
/// point it at a [`wiremock`] mock server.
#[derive(Debug, Clone)]
pub struct GeminiClient {
    endpoint: String,
    api_key: String,
    http: reqwest::Client,
}

impl GeminiClient {
    /// Build a client from `GEMINI_API_KEY`. Returns `Err` if the env var is
    /// unset or empty — the supervisor logs a single warning and disables
    /// itself rather than retrying a broken config.
    pub fn from_env() -> Result<Self, String> {
        let key = std::env::var("GEMINI_API_KEY")
            .map_err(|_| "GEMINI_API_KEY not set".to_string())?;
        if key.trim().is_empty() {
            return Err("GEMINI_API_KEY empty".to_string());
        }
        Ok(Self::new(GEMINI_ENDPOINT.to_string(), key))
    }

    pub fn new(endpoint: String, api_key: String) -> Self {
        Self {
            endpoint,
            api_key,
            http: reqwest::Client::new(),
        }
    }

    /// Override the endpoint — used by `wiremock` tests so we can point at a
    /// local mock without touching env vars.
    pub fn with_endpoint(mut self, endpoint: String) -> Self {
        self.endpoint = endpoint;
        self
    }

    /// Single round-trip. Returns the raw text from
    /// `candidates[0].content.parts[0].text` — caller passes it to
    /// [`parse_classification`].
    pub async fn generate(&self, agent_chunk: &str) -> Result<String, String> {
        // Body shape per https://ai.google.dev/gemini-api/docs/text-generation
        // Gemma 3 supports system_instruction since GA.
        let body = serde_json::json!({
            "system_instruction": {
                "parts": [{"text": SYSTEM_PROMPT}]
            },
            "contents": [{
                "role": "user",
                "parts": [{"text": agent_chunk}]
            }],
            "generationConfig": {
                "responseMimeType": "application/json",
                "temperature": 0.0,
                "maxOutputTokens": 256
            }
        });

        // The API key goes in the `x-goog-api-key` header — keeps it out of
        // the request URL (which proxies / access logs commonly capture).
        let resp = self
            .http
            .post(&self.endpoint)
            .header("x-goog-api-key", &self.api_key)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                // Strip URL (may include endpoint host) and any accidental
                // body capture. reqwest::Error never includes header values
                // by design, but we still scrub the key just in case.
                redact_secret(&format!("gemini http: {}", e), &self.api_key)
            })?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| redact_secret(&format!("gemini body: {}", e), &self.api_key))?;
        if !status.is_success() {
            return Err(redact_secret(
                &format!("gemini status {}: {}", status, text),
                &self.api_key,
            ));
        }
        // Pull `candidates[0].content.parts[0].text`.
        let v: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("gemini json: {}", e))?;
        let text = v
            .pointer("/candidates/0/content/parts/0/text")
            .and_then(|x| x.as_str())
            .ok_or_else(|| "gemini: empty candidates".to_string())?;
        Ok(text.to_string())
    }
}

/// One-shot convenience: classify `chunk` with the given client. Errors are
/// already redacted.
pub async fn classify_chunk(
    client: &GeminiClient,
    chunk: &str,
) -> Result<Classification, String> {
    let raw = client.generate(chunk).await?;
    parse_classification(&raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_happy_decision() {
        let raw = r#"{"kind":"decision_request","prompt_text":"Continue?","options":["yes","no"]}"#;
        let c = parse_classification(raw).unwrap();
        assert_eq!(c.kind, ClassifierKind::DecisionRequest);
        assert_eq!(c.prompt_text.as_deref(), Some("Continue?"));
        assert_eq!(c.options, vec!["yes".to_string(), "no".to_string()]);
    }

    #[test]
    fn parse_happy_noise_no_options_field() {
        let raw = r#"{"kind":"noise","prompt_text":null}"#;
        let c = parse_classification(raw).unwrap();
        assert_eq!(c.kind, ClassifierKind::Noise);
        assert!(c.prompt_text.is_none());
        assert!(c.options.is_empty());
    }

    #[test]
    fn parse_strips_markdown_fence() {
        let raw = "```json\n{\"kind\":\"noise\",\"prompt_text\":null,\"options\":[]}\n```";
        let c = parse_classification(raw).unwrap();
        assert_eq!(c.kind, ClassifierKind::Noise);
    }

    #[test]
    fn parse_empty_prompt_text_becomes_none() {
        let raw = r#"{"kind":"noise","prompt_text":"  ","options":[]}"#;
        let c = parse_classification(raw).unwrap();
        assert!(c.prompt_text.is_none());
    }

    #[test]
    fn parse_malformed_returns_err() {
        // truncated JSON
        assert!(parse_classification("{\"kind\":\"deci").is_err());
        // wrong shape
        assert!(parse_classification("[1,2,3]").is_err());
        // empty string
        assert!(parse_classification("").is_err());
        // unknown kind
        assert!(parse_classification(r#"{"kind":"???"}"#).is_err());
    }

    #[test]
    fn redact_secret_replaces_key() {
        let out = redact_secret("oops endpoint?key=AIza-SECRET", "AIza-SECRET");
        assert!(!out.contains("AIza-SECRET"));
        assert!(out.contains("[REDACTED]"));
    }

    #[test]
    fn redact_secret_empty_secret_is_noop() {
        let out = redact_secret("nothing to scrub", "");
        assert_eq!(out, "nothing to scrub");
    }
}
