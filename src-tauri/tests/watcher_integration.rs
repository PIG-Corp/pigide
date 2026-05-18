//! Watcher integration tests.
//!
//! Verifies:
//! 1. Mocked HTTP roundtrip: a `wiremock` server returns a Gemini-shaped
//!    body, [`GeminiClient`] sends the request and [`classify_chunk`]
//!    parses the verdict.
//! 2. The API key is sent in the `x-goog-api-key` header (NOT the URL) and
//!    never appears in the request URL the mock observes.
//! 3. `redact_secret` scrubs the key from any error string.

#![cfg(feature = "watcher")]

use pigide_lib::watcher::{
    classifier::{endpoint_for, redact_secret, GeminiClient, DEFAULT_MODEL},
    classify_chunk, ClassifierKind,
};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const API_KEY: &str = "AIza-FAKE-KEY-FOR-TESTS-DO-NOT-USE";

fn gemini_response(body: &str) -> serde_json::Value {
    serde_json::json!({
        "candidates": [{
            "content": {
                "parts": [{"text": body}],
                "role": "model"
            },
            "finishReason": "STOP"
        }]
    })
}

fn endpoint(server: &MockServer) -> String {
    format!(
        "{}/v1beta/models/{}:generateContent",
        server.uri(),
        DEFAULT_MODEL
    )
}

fn endpoint_path() -> String {
    format!("/v1beta/models/{}:generateContent", DEFAULT_MODEL)
}

#[tokio::test]
async fn classify_chunk_decision_request_via_mock() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path(endpoint_path()))
        .and(header("x-goog-api-key", API_KEY))
        .respond_with(ResponseTemplate::new(200).set_body_json(gemini_response(
            r#"{"kind":"decision_request","prompt_text":"Run migrations now?","options":["yes","no"]}"#,
        )))
        .mount(&server)
        .await;

    let client = GeminiClient::new(endpoint(&server), API_KEY.to_string());

    let v = classify_chunk(&client, "Run migrations now? (y/N) ").await.unwrap();
    assert_eq!(v.kind, ClassifierKind::DecisionRequest);
    assert_eq!(v.prompt_text.as_deref(), Some("Run migrations now?"));
    assert_eq!(v.options, vec!["yes".to_string(), "no".to_string()]);
}

#[tokio::test]
async fn classify_chunk_noise_via_mock() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(gemini_response(
            r#"{"kind":"noise","prompt_text":null,"options":[]}"#,
        )))
        .mount(&server)
        .await;

    let client = GeminiClient::new(endpoint(&server), API_KEY.to_string());
    let v = classify_chunk(&client, "compiling crate foo\n").await.unwrap();
    assert_eq!(v.kind, ClassifierKind::Noise);
}

#[tokio::test]
async fn classify_chunk_salvages_gemma_prose() {
    // Gemma 4 IT often emits reasoning prose around the JSON. The salvage
    // path in `parse_classification` must recover the verdict so a
    // misbehaving model doesn't reduce the Watcher to "always noise".
    let server = MockServer::start().await;
    let prose_response = "*   Analysis: yes/no prompt blocking the agent.\n\n\
                          {\"kind\":\"decision_request\",\"prompt_text\":\"Continue?\",\"options\":[\"y\",\"N\"]}\n\n\
                          End.";
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(gemini_response(prose_response)),
        )
        .mount(&server)
        .await;

    let client = GeminiClient::new(endpoint(&server), API_KEY.to_string());
    let v = classify_chunk(&client, "Continue? (y/N) ").await.unwrap();
    assert_eq!(v.kind, ClassifierKind::DecisionRequest);
    assert_eq!(v.prompt_text.as_deref(), Some("Continue?"));
}

#[tokio::test]
async fn api_key_never_in_url() {
    // Capture every request the mock saw and confirm no recorded URL or
    // headers leak the API key in plaintext to the wrong place.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(gemini_response(
            r#"{"kind":"noise","options":[]}"#,
        )))
        .mount(&server)
        .await;

    let client = GeminiClient::new(endpoint(&server), API_KEY.to_string());
    let _ = classify_chunk(&client, "hello").await.unwrap();

    let received: Vec<Request> = server.received_requests().await.unwrap();
    assert_eq!(received.len(), 1);
    let r = &received[0];
    assert!(
        !r.url.to_string().contains(API_KEY),
        "API key leaked into request URL: {}",
        r.url
    );
    // Header is the only place the key may appear.
    let header_val = r.headers.get("x-goog-api-key").and_then(|v| v.to_str().ok());
    assert_eq!(header_val, Some(API_KEY));
}

#[tokio::test]
async fn http_error_redacts_api_key() {
    // Force a 500 so the error path runs; verify the returned error string
    // does not contain the API key.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500).set_body_string("upstream boom"))
        .mount(&server)
        .await;

    let client = GeminiClient::new(endpoint(&server), API_KEY.to_string());
    let err = classify_chunk(&client, "hello").await.unwrap_err();
    assert!(
        !err.contains(API_KEY),
        "API key leaked into error string: {}",
        err
    );
    assert!(err.contains("[REDACTED]") || !err.contains("AIza"));
}

#[test]
fn redact_secret_handles_long_strings() {
    let needle = "AIza-FAKE-KEY";
    let haystack = format!("oops: GET ?key={} 401", needle);
    let cleaned = redact_secret(&haystack, needle);
    assert!(!cleaned.contains(needle));
    assert!(cleaned.contains("[REDACTED]"));
}

#[test]
fn endpoint_is_current_default_model() {
    // Sanity check — guards against accidental rename of the model
    // constant. `gemma-4-31b-it` is the Gemma 4 IT replacement for the
    // retired `gemma-3-4b-it`; override at runtime via PIGIDE_WATCHER_MODEL.
    let url = endpoint_for(DEFAULT_MODEL);
    assert!(url.contains(":generateContent"));
    assert!(url.contains("/v1beta/models/"));
    assert!(url.contains(DEFAULT_MODEL));
}
