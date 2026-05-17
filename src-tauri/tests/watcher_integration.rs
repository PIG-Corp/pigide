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
    classifier::{redact_secret, GeminiClient, GEMINI_ENDPOINT},
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

#[tokio::test]
async fn classify_chunk_decision_request_via_mock() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1beta/models/gemma-3-4b-it:generateContent"))
        .and(header("x-goog-api-key", API_KEY))
        .respond_with(ResponseTemplate::new(200).set_body_json(gemini_response(
            r#"{"kind":"decision_request","prompt_text":"Run migrations now?","options":["yes","no"]}"#,
        )))
        .mount(&server)
        .await;

    let endpoint = format!(
        "{}/v1beta/models/gemma-3-4b-it:generateContent",
        server.uri()
    );
    let client = GeminiClient::new(endpoint, API_KEY.to_string());

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

    let client = GeminiClient::new(
        format!("{}/v1beta/models/gemma-3-4b-it:generateContent", server.uri()),
        API_KEY.to_string(),
    );
    let v = classify_chunk(&client, "compiling crate foo\n").await.unwrap();
    assert_eq!(v.kind, ClassifierKind::Noise);
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

    let client = GeminiClient::new(
        format!("{}/v1beta/models/gemma-3-4b-it:generateContent", server.uri()),
        API_KEY.to_string(),
    );
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

    let client = GeminiClient::new(
        format!("{}/v1beta/models/gemma-3-4b-it:generateContent", server.uri()),
        API_KEY.to_string(),
    );
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
fn endpoint_is_gemma_3_4b_it() {
    // Sanity check — guards against accidental rename of the constant
    // that would silently route traffic to the wrong model.
    assert!(GEMINI_ENDPOINT.contains("gemma-3-4b-it:generateContent"));
}
