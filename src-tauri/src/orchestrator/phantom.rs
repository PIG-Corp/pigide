//! Phantom tool_call detector.
//!
//! Catches the failure mode where the model narrates an action ("I'll send
//! the prompt to the agent", "сейчас вызову search") but emits no
//! `tool_calls`. The caller (the orchestrator tool loop) re-prompts once
//! with [`RETRY_NAG`]; a second failure surfaces a warning to the user.
//!
//! The detector is intentionally a pure function over `(content, has_tools)`
//! so it can be unit-tested without spinning up a provider.

use serde::Serialize;
use std::path::{Path, PathBuf};

/// Bilingual trigger phrases that indicate the model *described* a tool
/// call instead of emitting one. Lowercased — the detector lowercases the
/// content before matching. Keep in sync with the prompt's "forbidden
/// phrases" list in `prompt.rs`.
const TRIGGER_PHRASES: &[&str] = &[
    // English
    "i called",
    "i will call",
    "i'll call",
    "i'll send",
    "i will send",
    "sending to agent",
    "sending the prompt",
    "let me run",
    "let me invoke",
    "let me call",
    "let me send",
    // Russian
    "вызвал тулз",
    "вызвала тулз",
    "вызвал тул",
    "отправил промт",
    "отправила промт",
    "отправил промпт",
    "отправила промпт",
    "закинул",
    "закинула",
    "сейчас вызову",
    "сейчас отправлю",
    "сейчас закину",
];

/// Hard nag injected as a system message on the retry turn.
pub const RETRY_NAG: &str = "You described an action but emitted no tool_call. \
Emit the tool_call NOW. Do not narrate.";

/// Returns `true` when `content` contains a trigger phrase AND no
/// tool_calls were emitted. An empty content with tool_calls is fine; a
/// narrative content with tool_calls is also fine — only the *empty
/// tool_calls + narrative* combo is phantom.
pub fn is_phantom(content: &str, has_tool_calls: bool) -> bool {
    if has_tool_calls {
        return false;
    }
    let lower = content.to_lowercase();
    TRIGGER_PHRASES.iter().any(|p| lower.contains(p))
}

/// One JSONL row appended to `.pigmemory/phantom_log.jsonl`.
#[derive(Debug, Clone, Serialize)]
pub struct PhantomEvent<'a> {
    pub ts: String,
    pub model: &'a str,
    /// Up to 240 chars of the offending assistant `content`.
    pub snippet: String,
    /// `true` if this row records the *original* phantom turn that we
    /// re-prompted; `false` if it records the second-failure case.
    pub retried: bool,
    /// `true` if the retry produced a real `tool_call` (or a clean
    /// no-op text reply); `false` if the retry also failed.
    pub resolved: bool,
}

impl<'a> PhantomEvent<'a> {
    pub fn new(model: &'a str, content: &str, retried: bool, resolved: bool) -> Self {
        let snippet: String = content.chars().take(240).collect();
        Self {
            ts: chrono::Utc::now().to_rfc3339(),
            model,
            snippet,
            retried,
            resolved,
        }
    }
}

/// Append one event as a JSONL line to `<root>/phantom_log.jsonl`. Best
/// effort — log errors are swallowed so a write failure can never
/// destabilise the orchestrator turn.
pub fn append_event(root: &Path, ev: &PhantomEvent<'_>) {
    let path = root.join("phantom_log.jsonl");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(line) = serde_json::to_string(ev) else {
        return;
    };
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{}", line);
    }
}

/// Resolve the log directory: prefer `<workspace.paths[0]>/.pigmemory/`,
/// else fall back to `./.pigmemory/` next to the running binary's CWD.
pub fn resolve_log_root(workspace_path: Option<&str>) -> PathBuf {
    if let Some(p) = workspace_path.filter(|s| !s.is_empty()) {
        return PathBuf::from(p).join(".pigmemory");
    }
    PathBuf::from(".pigmemory")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phantom_when_narrative_and_no_tools() {
        assert!(is_phantom("I'll send to agent", false));
        assert!(is_phantom("Sending to agent now.", false));
        assert!(is_phantom("Сейчас отправлю задание builder", false));
        assert!(is_phantom("Закинул промт kiro-cli", false));
        assert!(is_phantom("I called search_memories for you.", false));
    }

    #[test]
    fn not_phantom_when_tool_calls_present_even_if_empty_content() {
        assert!(!is_phantom("", true));
    }

    #[test]
    fn not_phantom_when_narrative_but_tool_calls_emitted() {
        assert!(!is_phantom("Sending now", true));
        assert!(!is_phantom("I'll send to agent", true));
    }

    #[test]
    fn not_phantom_for_neutral_text_without_tools() {
        assert!(!is_phantom("Готово. Workspace переименован.", false));
        assert!(!is_phantom("Done. Builder finished cleanly.", false));
        assert!(!is_phantom("", false));
    }

    #[test]
    fn case_insensitive_match() {
        assert!(is_phantom("I'LL SEND THE PROMPT NOW", false));
        assert!(is_phantom("LET ME RUN search", false));
    }

    #[test]
    fn snippet_truncated_to_240_chars() {
        let long = "x".repeat(1000);
        let ev = PhantomEvent::new("m", &long, true, false);
        assert_eq!(ev.snippet.chars().count(), 240);
    }

    #[test]
    fn append_event_writes_jsonl_line() {
        let dir = std::env::temp_dir()
            .join(format!("pigide-phantom-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let ev = PhantomEvent::new("kr/claude-opus-4.7", "I'll send", true, true);
        append_event(&dir, &ev);
        let path = dir.join("phantom_log.jsonl");
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.ends_with('\n'));
        let parsed: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
        assert_eq!(parsed["model"], "kr/claude-opus-4.7");
        assert_eq!(parsed["retried"], true);
        assert_eq!(parsed["resolved"], true);
        assert_eq!(parsed["snippet"], "I'll send");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_log_root_prefers_workspace() {
        let p = resolve_log_root(Some("/tmp/proj"));
        assert_eq!(p, PathBuf::from("/tmp/proj/.pigmemory"));
        let p2 = resolve_log_root(None);
        assert_eq!(p2, PathBuf::from(".pigmemory"));
        let p3 = resolve_log_root(Some(""));
        assert_eq!(p3, PathBuf::from(".pigmemory"));
    }
}
