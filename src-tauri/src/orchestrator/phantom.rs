//! Phantom tool_call detector.
//!
//! Catches the failure mode where the model narrates an action ("I'll send
//! the prompt to the agent", "сейчас вызову search", `Calling tools:\n  -
//! spawn_agent(...)`) but emits no `tool_calls`. The caller (the
//! orchestrator tool loop) re-prompts up to `max_phantom_retries` times
//! with [`RETRY_NAG`]; further failures surface a warning to the user.
//!
//! The detector is intentionally a pure function over `(content, has_tools)`
//! so it can be unit-tested without spinning up a provider.

use once_cell::sync::Lazy;
use regex::RegexSet;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Bilingual narration patterns. Compiled as a single
/// case-insensitive multiline `RegexSet`. Keep in sync with the prompt's
/// "forbidden phrases" list in `prompt.rs`. ORDER MATTERS for
/// `matched_pattern_index` — append, don't reorder.
pub const NARRATION_PATTERNS: &[&str] = &[
    // 0: literal `Calling tools:` / `Calling tool:` line — most common
    //    OmniRouter failure mode where the model echoes the round-trip
    //    history format instead of emitting a real tool_call.
    r"(?m)^\s*calling\s+tools?\s*:",
    // 1: I'll / I will <verb> the agent / a tool
    r"\bi['\u{2019}]?ll\s+(call|run|send|invoke|spawn|dispatch|use)\b",
    r"\bi\s+will\s+(call|run|send|invoke|spawn|dispatch|use)\b",
    // 3: let me <verb>
    r"\blet\s+me\s+(call|run|invoke|send|spawn|dispatch)\b",
    // 4: sending to / the agent / the prompt
    r"\bsending\s+(to|the)\s+(agent|builder|prompt|task|brief)\b",
    // 5: past-tense "I called X for you"
    r"\bi\s+called\s+\w+",
    // 6 RU: вызвал/вызвала тулз[ыу]
    r"вызвал[аи]?\s+тулз",
    // 7 RU: вызвал тул (короче)
    r"вызвал[аи]?\s+тул\b",
    // 8 RU: отправил/отправила промт/промпт
    r"отправил[аи]?\s+пром(п)?т",
    // 9 RU: закинул/закинула задание/промт/брифа
    r"закинул[аи]?\s+(задани|промт|промпт|бриф)",
    // 10 RU: выдал/выдала бриф / задание
    r"выдал[аи]?\s+(бриф|задани|промт|промпт)",
    // 11 RU: сейчас + (вызову/отправлю/запущу/закину/выдам)
    r"сейчас\s+(вызову|отправлю|запущу|закину|выдам|спавн|спаун)",
];

static NARRATION_RE: Lazy<RegexSet> = Lazy::new(|| {
    // Compile case-insensitively. The `(?m)` inline flag on pattern 0
    // takes care of multiline anchors; setting `case_insensitive` on the
    // builder makes the rest of the list bilingual-friendly.
    regex::RegexSetBuilder::new(NARRATION_PATTERNS)
        .case_insensitive(true)
        .build()
        .expect("phantom narration regex set must compile")
});

/// Hard nag injected as a system message on the retry turn.
pub const RETRY_NAG: &str = "You described a tool action but emitted no tool_call. \
Re-issue this turn as a real tool_call NOW. No narration, no `Calling tools:` text — \
the function-call channel is the only thing that runs.";

/// Default cap on consecutive phantom re-prompts before surfacing a
/// visible warning to the user. Overridable via the
/// `orchestrator.max_phantom_retries` setting.
pub const DEFAULT_MAX_PHANTOM_RETRIES: u32 = 2;

/// Returns `true` when `content` matches a narration pattern AND no
/// tool_calls were emitted. An empty content with tool_calls is fine; a
/// narrative content with tool_calls is also fine — only the
/// *empty tool_calls + narrative* combo is phantom.
pub fn is_phantom(content: &str, has_tool_calls: bool) -> bool {
    if has_tool_calls {
        return false;
    }
    NARRATION_RE.is_match(content)
}

/// Returns the index (into [`NARRATION_PATTERNS`]) of the first matching
/// pattern, or `None`. Used in tests and structured logs to attribute the
/// detection to a specific rule.
pub fn matched_pattern(content: &str) -> Option<usize> {
    NARRATION_RE.matches(content).into_iter().next()
}

/// One JSONL row appended to `.pigmemory/phantom_log.jsonl`.
#[derive(Debug, Clone, Serialize)]
pub struct PhantomEvent<'a> {
    pub ts: String,
    pub model: &'a str,
    /// Up to 240 chars of the offending assistant `content`.
    pub snippet: String,
    /// Which pattern fired (index into [`NARRATION_PATTERNS`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern_index: Option<usize>,
    /// Retry attempt number, 1-based. `0` = original detection,
    /// `>=1` = re-prompts.
    pub attempt: u32,
    /// `true` if the retry produced a real `tool_call` (or a clean
    /// no-op text reply); `false` if this row records a still-failing
    /// turn.
    pub resolved: bool,
}

impl<'a> PhantomEvent<'a> {
    pub fn new(model: &'a str, content: &str, attempt: u32, resolved: bool) -> Self {
        let snippet: String = content.chars().take(240).collect();
        Self {
            ts: chrono::Utc::now().to_rfc3339(),
            model,
            snippet,
            pattern_index: matched_pattern(content),
            attempt,
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

    // --- One assertion per regex branch ---

    #[test]
    fn phantom_calling_tools_literal() {
        // pattern 0
        assert!(is_phantom("Calling tools:\n  - spawn_agent(...)", false));
        assert!(is_phantom("  Calling tool: search_memories", false));
        assert_eq!(
            matched_pattern("Calling tools:\n  - spawn_agent(...)"),
            Some(0)
        );
    }

    #[test]
    fn phantom_ill_verb() {
        // pattern 1
        assert!(is_phantom("I'll spawn a builder right after.", false));
        assert!(is_phantom("Ill run search_memories", false));
        assert!(is_phantom("I\u{2019}ll send the brief.", false));
    }

    #[test]
    fn phantom_i_will_verb() {
        // pattern 2
        assert!(is_phantom("I will invoke spawn_agent shortly.", false));
        assert!(is_phantom("Now I will dispatch the builder.", false));
    }

    #[test]
    fn phantom_let_me_verb() {
        // pattern 3
        assert!(is_phantom("Let me run search_memories", false));
        assert!(is_phantom("let me spawn three builders.", false));
    }

    #[test]
    fn phantom_sending_to_agent() {
        // pattern 4
        assert!(is_phantom("Sending to agent now.", false));
        assert!(is_phantom("sending the prompt to kiro-cli", false));
    }

    #[test]
    fn phantom_past_tense_i_called() {
        // pattern 5
        assert!(is_phantom("I called search_memories for you.", false));
    }

    #[test]
    fn phantom_ru_vyzval_tulz() {
        // pattern 6
        assert!(is_phantom("Вызвал тулзу search.", false));
        assert!(is_phantom("вызвала тулзы по очереди", false));
    }

    #[test]
    fn phantom_ru_vyzval_tul_short() {
        // pattern 7
        assert!(is_phantom("Вызвал тул search_memories", false));
    }

    #[test]
    fn phantom_ru_otpravil_promt() {
        // pattern 8
        assert!(is_phantom("Отправил промт kiro-cli", false));
        assert!(is_phantom("отправила промпт сборщику", false));
    }

    #[test]
    fn phantom_ru_zakinul_zadanie() {
        // pattern 9
        assert!(is_phantom("Закинул задание в builder", false));
        assert!(is_phantom("закинула промт kiro", false));
        assert!(is_phantom("закинул бриф ревьюверу", false));
    }

    #[test]
    fn phantom_ru_vydal_brief() {
        // pattern 10
        assert!(is_phantom("Выдал бриф билдеру", false));
        assert!(is_phantom("выдала задание", false));
    }

    #[test]
    fn phantom_ru_seichas_verb() {
        // pattern 11
        assert!(is_phantom("Сейчас вызову spawn_agent.", false));
        assert!(is_phantom("сейчас отправлю промт", false));
        assert!(is_phantom("сейчас запущу билдеров", false));
    }

    // --- Negative cases ---

    #[test]
    fn not_phantom_when_tool_calls_present_even_if_empty_content() {
        assert!(!is_phantom("", true));
    }

    #[test]
    fn not_phantom_when_narrative_but_tool_calls_emitted() {
        // The detector is a *gate*: if the function-call channel
        // actually fired, narration alongside is fine.
        assert!(!is_phantom("Sending now", true));
        assert!(!is_phantom("I'll send to agent", true));
        assert!(!is_phantom("Calling tools:\n  - spawn_agent", true));
    }

    #[test]
    fn not_phantom_for_neutral_text_without_tools() {
        assert!(!is_phantom("Готово. Workspace переименован.", false));
        assert!(!is_phantom("Done. Builder finished cleanly.", false));
        assert!(!is_phantom("", false));
        // Bare verb without subject — must not false-positive.
        assert!(!is_phantom("The build will spawn child processes.", false));
    }

    #[test]
    fn case_insensitive_match() {
        assert!(is_phantom("I'LL SEND THE PROMPT NOW", false));
        assert!(is_phantom("LET ME RUN search", false));
        assert!(is_phantom("CALLING TOOLS:", false));
    }

    // --- Event row plumbing ---

    #[test]
    fn snippet_truncated_to_240_chars() {
        let long = "x".repeat(1000);
        let ev = PhantomEvent::new("m", &long, 0, false);
        assert_eq!(ev.snippet.chars().count(), 240);
    }

    #[test]
    fn event_records_pattern_index() {
        let ev = PhantomEvent::new(
            "kr/claude-opus-4.7",
            "Calling tools:\n  - spawn_agent",
            0,
            false,
        );
        assert_eq!(ev.pattern_index, Some(0));
        assert_eq!(ev.attempt, 0);
    }

    #[test]
    fn append_event_writes_jsonl_line() {
        let dir = std::env::temp_dir()
            .join(format!("pigide-phantom-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let ev = PhantomEvent::new("kr/claude-opus-4.7", "I'll send", 1, true);
        append_event(&dir, &ev);
        let path = dir.join("phantom_log.jsonl");
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.ends_with('\n'));
        let parsed: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
        assert_eq!(parsed["model"], "kr/claude-opus-4.7");
        assert_eq!(parsed["attempt"], 1);
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
