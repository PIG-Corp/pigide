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

/// User-turn prompt sent to Gemma. Gemma 4 IT does NOT honour the Gemini
/// `system_instruction` field cleanly and will 500 on
/// `responseMimeType: application/json`, so the classifier instead inlines
/// the contract into a single user turn and post-processes the reply with
/// [`extract_first_json_object`].
pub const PROMPT_PREFIX: &str = "\
You classify a single chunk of stdout from an autonomous CLI coding agent. \
Decide whether the agent is asking a human for a decision (yes/no, pick an \
option, supply a value) or just printing progress / errors / noise.

Return a single JSON object on one line and nothing else. No prose, no \
markdown fences, no analysis. Schema:
{\"kind\":\"decision_request\"|\"noise\",\"prompt_text\":string|null,\"options\":[string]}

Rules:
- \"decision_request\" only when the very last visible line is a prompt the \
  agent is BLOCKED on (e.g. '(y/N)', numbered choice, '> ', 'Enter \
  password').
- Plain progress, errors without a prompt, ANSI repaints, banners → \"noise\".
- prompt_text: the question reduced to one short sentence, or null for noise.
- options: discrete choices the agent surfaced (in order); [] if free-form \
  or noise.

Chunk:
---
";

/// Suffix appended after the chunk to bias the model toward emitting the
/// JSON immediately.
pub const PROMPT_SUFFIX: &str = "\n---\n\nJSON:";

/// Default model.
///
/// The original brief named `gemma-3-4b-it`, but Google AI Studio retired
/// that variant under v1beta. Two replacements were trialled live:
/// * `gemma-4-31b-it` — accepts the request but Gemma 4 IT narrates its
///   reasoning and never emits a clean `{...}` block, so the classifier
///   could not recover a verdict reliably even with a salvage parser.
/// * `gemini-2.5-flash-lite` — obeys "respond with JSON only" out of the
///   box, runs on the same v1beta endpoint, and costs roughly 10× less
///   per token than the Gemma 4 IT family.
///
/// Flash-Lite is therefore the default. Override with the
/// `PIGIDE_WATCHER_MODEL` env var if your project has a Gemma variant
/// available (e.g. via Vertex AI rather than AI Studio).
pub const DEFAULT_MODEL: &str = "gemini-2.5-flash-lite";

/// Endpoint template — `{model}` is substituted at call time. Overridable
/// via [`GeminiClient::with_endpoint`] for tests.
pub const GEMINI_ENDPOINT_TEMPLATE: &str =
    "https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent";

/// Strip a candidate `?key=...` query param from any string. Used in error
/// paths so a panic / log call cannot accidentally surface the API key.
pub fn redact_secret(input: &str, secret: &str) -> String {
    if secret.is_empty() {
        return input.to_string();
    }
    input.replace(secret, "[REDACTED]")
}

/// Parse Gemini's `candidates[0].content.parts[0].text` payload into a
/// [`Classification`].
///
/// Gemma 4 IT models (the current AI-Studio replacement for `gemma-3-4b-it`)
/// frequently emit reasoning prose around the JSON, even when explicitly
/// told not to and even with `responseMimeType: application/json`. To stay
/// robust without yielding accuracy, this parser:
///
/// 1. trims a leading/trailing markdown fence (```json … ```);
/// 2. if direct `serde_json::from_str` fails, scans the text for the first
///    balanced `{ … }` block at brace depth 0 and tries that;
/// 3. normalizes `prompt_text: ""` / whitespace-only → `None`.
///
/// Returns `Err` on any other malformed payload — caller treats that as
/// "drop this chunk, it's noise".
pub fn parse_classification(raw: &str) -> Result<Classification, String> {
    let cleaned = strip_json_fence(raw.trim());
    let value: Result<Classification, _> = serde_json::from_str(cleaned);
    let mut value = match value {
        Ok(v) => v,
        Err(direct_err) => {
            // Salvage path: pull the first balanced JSON object out of the
            // text and try again. Saves us from prose-prefixed Gemma replies.
            match extract_first_json_object(cleaned) {
                Some(obj) => serde_json::from_str(&obj)
                    .map_err(|e| format!("classifier JSON salvage: {}", e))?,
                None => return Err(format!("classifier JSON parse: {}", direct_err)),
            }
        }
    };
    // Normalize empty strings -> None to keep downstream code simple.
    if let Some(s) = value.prompt_text.as_ref() {
        if s.trim().is_empty() {
            value.prompt_text = None;
        }
    }
    Ok(value)
}

/// Scan `s` for the first balanced `{ … }` block at brace-depth 0, ignoring
/// braces inside double-quoted strings (with backslash escapes). Returns the
/// object as an owned `String` so the caller can pass it to `from_str`.
fn extract_first_json_object(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut start: Option<usize> = None;
    let mut depth: i32 = 0;
    let mut in_str = false;
    let mut escape = false;
    for (i, &b) in bytes.iter().enumerate() {
        if in_str {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => {
                if depth == 0 {
                    start = Some(i);
                }
                depth += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    if let Some(s0) = start {
                        return Some(s[s0..=i].to_string());
                    }
                }
            }
            _ => {}
        }
    }
    None
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

/// Build the default endpoint for `model` by substituting it into
/// [`GEMINI_ENDPOINT_TEMPLATE`].
pub fn endpoint_for(model: &str) -> String {
    GEMINI_ENDPOINT_TEMPLATE.replace("{model}", model)
}

impl GeminiClient {
    /// Build a client from `GEMINI_API_KEY` (and optionally
    /// `PIGIDE_WATCHER_MODEL` to override the default model). Returns `Err`
    /// if the API key is unset or empty — the supervisor logs a single
    /// warning and disables itself rather than retrying a broken config.
    pub fn from_env() -> Result<Self, String> {
        let key =
            std::env::var("GEMINI_API_KEY").map_err(|_| "GEMINI_API_KEY not set".to_string())?;
        if key.trim().is_empty() {
            return Err("GEMINI_API_KEY empty".to_string());
        }
        let model = std::env::var("PIGIDE_WATCHER_MODEL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_MODEL.to_string());
        Ok(Self::new(endpoint_for(&model), key))
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
        // — Gemini 2.x Flash-Lite (the default) supports `responseMimeType:
        // application/json`. Some Gemma variants do not (they 500 on it),
        // so we keep the request shape narrow: a single user turn that
        // inlines the contract, plus `responseMimeType` which Flash-Lite
        // honours and Gemma simply ignores. The salvage path in
        // `parse_classification` keeps the parser working even when the
        // model decides to narrate.
        let prompt = format!("{}{}{}", PROMPT_PREFIX, agent_chunk, PROMPT_SUFFIX);
        let body = serde_json::json!({
            "contents": [{
                "role": "user",
                "parts": [{"text": prompt}]
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
pub async fn classify_chunk(client: &GeminiClient, chunk: &str) -> Result<Classification, String> {
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
    fn parse_salvages_json_from_gemma_prose() {
        // Real Gemma 4 31B IT output — model wraps the JSON in reasoning.
        // Salvage path must still recover the verdict.
        let raw = "*   Analysis: this is a yes/no prompt.\n\
                   *   Classification:\n\n\
                   {\"kind\":\"decision_request\",\"prompt_text\":\"Continue?\",\"options\":[\"y\",\"N\"]}\n\
                   *   Done.";
        let c = parse_classification(raw).unwrap();
        assert_eq!(c.kind, ClassifierKind::DecisionRequest);
        assert_eq!(c.prompt_text.as_deref(), Some("Continue?"));
        assert_eq!(c.options, vec!["y".to_string(), "N".to_string()]);
    }

    #[test]
    fn parse_salvages_first_object_only() {
        // If the prose contains a stray `{}` before the real verdict, we
        // pick the first balanced object — caller has to write a sane
        // prompt; we only need this to not panic and to parse a real one.
        let raw = "intro {\"kind\":\"noise\",\"options\":[]} trailing prose";
        let c = parse_classification(raw).unwrap();
        assert_eq!(c.kind, ClassifierKind::Noise);
    }

    #[test]
    fn extract_first_json_object_handles_nesting_and_strings() {
        let s = "x{\"a\":\"}{\",\"b\":{\"c\":1}}y";
        let got = extract_first_json_object(s).unwrap();
        // Must consume to the matching outer `}`, not stop at the brace
        // inside the string.
        assert_eq!(got, "{\"a\":\"}{\",\"b\":{\"c\":1}}");
    }

    #[test]
    fn endpoint_for_substitutes_model() {
        let url = endpoint_for("gemma-4-31b-it");
        assert!(url.ends_with("gemma-4-31b-it:generateContent"));
        assert!(url.contains("/v1beta/models/"));
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
