//! Pure prompt builder + response parser for the smart-lane LLM.
//!
//! No I/O — just turn `(items, existing_slugs)` into a chat-completions
//! payload and turn the model's reply text into structured upserts/edits
//! the worker can apply.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// One row passed in to the LLM. The worker hydrates these from the
/// fast-lane stubs before building the prompt.
#[derive(Debug, Clone, Serialize)]
pub struct BatchItem {
    pub queue_id: i64,
    pub kind: String,      // "task_complete" | "chat_chunk"
    pub note_slug: String, // e.g. "tasks/abc-123" or "chats/agent/2026-05-27"
    pub note_title: String,
    pub note_body: String, // truncated to ~4KB by caller
}

/// Existing note slug + title for the LLM to "prefer linking to existing
/// over creating new". Capped by caller (typically top-50 by FTS score).
#[derive(Debug, Clone, Serialize)]
pub struct ExistingSlug {
    pub slug: String,
    pub title: String,
    pub kind: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Upsert {
    pub kind: String, // "concept" | "entity" | "source"
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub links_to_slugs: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Edit {
    pub slug: String,
    pub append_section: String,
    pub body: String,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct ParsedBatch {
    #[serde(default)]
    pub upsert: Vec<Upsert>,
    #[serde(default)]
    pub edits: Vec<Edit>,
}

const SYSTEM_PROMPT: &str = include_str!("./prompt_system.txt");

/// Build the chat-completions `messages` array for the smart-lane.
pub fn build_messages(
    workspace_name: &str,
    items: &[BatchItem],
    existing: &[ExistingSlug],
    max_new: usize,
) -> Vec<Value> {
    let user_payload = json!({
        "workspace_name": workspace_name,
        "max_new_notes": max_new,
        "existing_slugs": existing,
        "items": items,
    });
    vec![
        json!({ "role": "system", "content": SYSTEM_PROMPT }),
        json!({
            "role": "user",
            "content": serde_json::to_string_pretty(&user_payload).unwrap_or_else(|_| "{}".into()),
        }),
    ]
}

/// Parse the model's reply. Strips ```json fences and leading prose so a
/// chatty model can still produce valid output. Returns Err on JSON
/// errors or schema mismatches; the worker treats this as a per-batch
/// failure and bumps `smart_attempts` for every row in the batch.
pub fn parse_response(text: &str) -> Result<ParsedBatch> {
    let json_text = extract_json_block(text)
        .ok_or_else(|| Error::Other("no JSON object in response".into()))?;
    let parsed: ParsedBatch =
        serde_json::from_str(json_text).map_err(|e| Error::Other(format!("parse: {}", e)))?;
    Ok(parsed)
}

fn extract_json_block(s: &str) -> Option<&str> {
    // 1. ```json ... ``` fence
    if let Some(start) = s.find("```json") {
        let after = &s[start + "```json".len()..];
        if let Some(end) = after.find("```") {
            let inner = after[..end].trim_start_matches('\n');
            return Some(inner);
        }
    }
    // 2. Plain ``` ... ``` fence
    if let Some(start) = s.find("```") {
        let after = &s[start + 3..];
        if let Some(end) = after.find("```") {
            let inner = after[..end].trim_start_matches('\n');
            return Some(inner);
        }
    }
    // 3. First `{` to last `}`
    let first = s.find('{')?;
    let last = s.rfind('}')?;
    if last > first {
        Some(&s[first..=last])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_items() -> Vec<BatchItem> {
        vec![BatchItem {
            queue_id: 1,
            kind: "task_complete".into(),
            note_slug: "tasks/abc-123".into(),
            note_title: "Wire ingest".into(),
            note_body: "## Summary\n\nAdd hook.\n".into(),
        }]
    }

    #[test]
    fn build_messages_includes_system_and_user_payload() {
        let msgs = build_messages("my-ws", &sample_items(), &[], 5);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[1]["role"], "user");
        let user_text = msgs[1]["content"].as_str().unwrap();
        assert!(user_text.contains("my-ws"));
        assert!(user_text.contains("tasks/abc-123"));
        assert!(user_text.contains("Add hook"));
    }

    #[test]
    fn parse_response_accepts_clean_json() {
        let raw = r#"{"upsert": [{"kind": "concept", "title": "Idempotent upsert", "body": "x", "tags": ["pattern"], "links_to_slugs": ["tasks/abc-123"]}], "edits": []}"#;
        let parsed = parse_response(raw).unwrap();
        assert_eq!(parsed.upsert.len(), 1);
        assert_eq!(parsed.upsert[0].kind, "concept");
        assert_eq!(parsed.upsert[0].title, "Idempotent upsert");
        assert_eq!(parsed.edits.len(), 0);
    }

    #[test]
    fn parse_response_strips_json_fence() {
        let raw = "Here you go:\n```json\n{\"upsert\": [], \"edits\": []}\n```\n";
        let parsed = parse_response(raw).unwrap();
        assert_eq!(parsed, ParsedBatch::default());
    }

    #[test]
    fn parse_response_strips_plain_fence() {
        let raw = "```\n{\"upsert\": [], \"edits\": []}\n```";
        let parsed = parse_response(raw).unwrap();
        assert_eq!(parsed, ParsedBatch::default());
    }

    #[test]
    fn parse_response_falls_back_to_first_brace_to_last() {
        let raw = "blabla {\"upsert\": [{\"kind\":\"entity\",\"title\":\"T\",\"body\":\"b\"}], \"edits\": []} trailing";
        let parsed = parse_response(raw).unwrap();
        assert_eq!(parsed.upsert.len(), 1);
        assert_eq!(parsed.upsert[0].kind, "entity");
    }

    #[test]
    fn parse_response_errors_on_garbage() {
        assert!(parse_response("plain text without braces").is_err());
        assert!(parse_response("{not valid json}").is_err());
    }

    #[test]
    fn parse_response_accepts_edit_appends() {
        let raw = r###"{"upsert": [], "edits": [{"slug": "tasks/abc-123", "append_section": "## Concepts referenced", "body": "- [[idempotent-upsert]]"}]}"###;
        let parsed = parse_response(raw).unwrap();
        assert_eq!(parsed.edits.len(), 1);
        assert_eq!(parsed.edits[0].slug, "tasks/abc-123");
        assert!(parsed.edits[0].body.contains("[[idempotent-upsert]]"));
    }
}
