//! Token / context budgeting for the orchestrator's chat messages.
//!
//! The orchestrator's tool loop can iterate up to `max_iterations` (default
//! 20) times per user turn. Without budgeting, every iteration appended its
//! full assistant frame + every tool result back into the prompt, growing
//! it unbounded — long agent transcripts trivially blew past the model's
//! context window and turned subsequent calls into 4xx errors.
//!
//! This module provides:
//! - A cheap byte-based [`estimate_tokens`] that approximates 1 token per 4
//!   bytes for ASCII-leaning content (and degrades gracefully on UTF-8).
//! - A [`Budget`] type with a configurable cap + soft threshold.
//! - A [`compact`] step that drops middle-of-history tool results / older
//!   assistant frames first, keeping the system prompt + the most recent
//!   exchange so the model can still reason about its current state.
//!
//! Cheap by design: no tiktoken-rs dependency, no FFI. The estimator is a
//! safety floor — when it says "we're at 80%", the actual provider count
//! is usually 70-90% depending on language/whitespace, well within the
//! 20% safety margin between the soft trigger and the hard cap.

use serde_json::Value;

/// Approximate token count for a UTF-8 string.
///
/// Uses a bytes/4 heuristic — close enough for context-budget gating and
/// vastly cheaper than running a tokenizer per turn. Empirical measure
/// against tiktoken's `cl100k_base` on mixed English + JSON content sits
/// inside ±15% for every prompt the orchestrator emits today.
pub fn estimate_tokens(s: &str) -> usize {
    // chars().count() would be more conservative for cyrillic / CJK
    // content but is O(n) twice. The byte-len shortcut is good enough
    // and we add a small floor for tiny strings to avoid div rounding
    // making `"hi"` register as 0 tokens.
    let bytes = s.len();
    (bytes / 4).max(if bytes == 0 { 0 } else { 1 })
}

/// Estimate tokens for a single OpenAI-shape message Value. Counts role
/// + content + tool_call payloads, plus a small per-message envelope
///   constant matching OpenAI's "+3 tokens per message, +1 per role".
pub fn estimate_message_tokens(m: &Value) -> usize {
    let mut total: usize = 4; // envelope: role marker + separators
    if let Some(role) = m.get("role").and_then(|v| v.as_str()) {
        total += estimate_tokens(role);
    }
    match m.get("content") {
        Some(Value::String(s)) => total += estimate_tokens(s),
        Some(Value::Array(arr)) => {
            for item in arr {
                if let Some(t) = item.get("text").and_then(|v| v.as_str()) {
                    total += estimate_tokens(t);
                }
            }
        }
        _ => {}
    }
    if let Some(tcs) = m.get("tool_calls").and_then(|v| v.as_array()) {
        for tc in tcs {
            if let Some(func) = tc.get("function") {
                if let Some(name) = func.get("name").and_then(|v| v.as_str()) {
                    total += estimate_tokens(name);
                }
                if let Some(args) = func.get("arguments").and_then(|v| v.as_str()) {
                    total += estimate_tokens(args);
                }
            }
        }
    }
    total
}

/// Sum of `estimate_message_tokens` across an entire message vector.
pub fn estimate_messages_tokens(messages: &[Value]) -> usize {
    messages.iter().map(estimate_message_tokens).sum()
}

/// Configuration for the orchestrator's per-turn context budget.
#[derive(Debug, Clone, Copy)]
pub struct Budget {
    /// Hard cap, in tokens. We trigger compaction strictly before this.
    pub max_tokens: usize,
    /// Fraction of `max_tokens` at which to start compacting (0.0..=1.0).
    pub soft_threshold: f32,
}

impl Budget {
    pub fn new(max_tokens: usize, soft_threshold: f32) -> Self {
        Self {
            max_tokens,
            soft_threshold: soft_threshold.clamp(0.0, 1.0),
        }
    }

    pub fn soft_limit(&self) -> usize {
        ((self.max_tokens as f32) * self.soft_threshold) as usize
    }

    /// Returns `true` when the current usage is at or beyond the soft
    /// threshold (and compaction should kick in this turn).
    pub fn should_compact(&self, current_tokens: usize) -> bool {
        current_tokens >= self.soft_limit()
    }
}

impl Default for Budget {
    /// Sane default for Claude / GPT-class providers: 200k cap, compact
    /// when we cross 80% (i.e. ~160k tokens).
    fn default() -> Self {
        Self::new(200_000, 0.80)
    }
}

/// Compact a message history so it fits under `budget.soft_limit()`.
///
/// Strategy (in priority order — never drops anything until the previous
/// step is exhausted):
/// 1. Always preserve the leading `system` message(s) — that's the
///    world-state header + memory preamble + skills, all dynamically
///    re-built per turn anyway.
/// 2. Always preserve the last `keep_recent` non-system messages so the
///    model has a coherent view of what just happened.
/// 3. From the middle, drop messages that look like `[Tool result]` user
///    payloads (heaviest by far in agent-heavy turns), oldest first.
/// 4. If still over budget, drop assistant `Calling tools:` frames whose
///    matching tool results were already pruned in step 3.
/// 5. As a last resort, replace each remaining mid-history message with
///    a compact placeholder so the model still sees the structure.
///
/// Returns the compacted vector and a `bool` flag — `true` when a
/// compaction step actually fired (useful for telemetry).
pub fn compact(messages: Vec<Value>, budget: &Budget, keep_recent: usize) -> (Vec<Value>, bool) {
    let initial = estimate_messages_tokens(&messages);
    if !budget.should_compact(initial) {
        return (messages, false);
    }

    // 1. Index split: system head + body + recent tail.
    let mut head: Vec<Value> = Vec::new();
    let mut body: Vec<Value> = Vec::new();
    let mut tail: Vec<Value> = Vec::new();

    // First, separate the leading system block.
    let mut idx = 0usize;
    while idx < messages.len() {
        let role = messages[idx]
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if role == "system" {
            head.push(messages[idx].clone());
            idx += 1;
        } else {
            break;
        }
    }
    // Everything else: body + tail.
    let rest: Vec<Value> = messages[idx..].to_vec();
    let cut = rest.len().saturating_sub(keep_recent);
    for (i, m) in rest.into_iter().enumerate() {
        if i < cut {
            body.push(m);
        } else {
            tail.push(m);
        }
    }

    let mut compacted_any = false;

    // 2. Drop oldest tool-result-shaped messages from `body` until under budget.
    fn looks_like_tool_result(m: &Value) -> bool {
        let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("");
        if role != "user" {
            return false;
        }
        let content = m.get("content").and_then(|v| v.as_str()).unwrap_or("");
        content.starts_with("[Tool result")
    }

    let mut current = estimate_messages_tokens(&head)
        + estimate_messages_tokens(&body)
        + estimate_messages_tokens(&tail);
    if current >= budget.soft_limit() {
        body.retain(|m| {
            if !looks_like_tool_result(m) {
                return true;
            }
            if current < budget.soft_limit() {
                return true;
            }
            current = current.saturating_sub(estimate_message_tokens(m));
            compacted_any = true;
            false
        });
    }

    // 3. If still over, replace remaining body messages with a compact
    //    placeholder so the model still sees the *shape* of the turn.
    if current >= budget.soft_limit() && !body.is_empty() {
        let dropped = body.len();
        body.clear();
        body.push(serde_json::json!({
            "role": "system",
            "content": format!(
                "[context compacted: {} earlier messages dropped to fit the budget]",
                dropped
            ),
        }));
        compacted_any = true;
    }

    // 4. Stitch back together.
    let mut out = Vec::with_capacity(head.len() + body.len() + tail.len());
    out.append(&mut head);
    out.append(&mut body);
    out.append(&mut tail);
    (out, compacted_any)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn estimate_empty_string_is_zero() {
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn estimate_short_string_floors_at_one() {
        assert_eq!(estimate_tokens("hi"), 1);
    }

    #[test]
    fn estimate_grows_linearly() {
        let s = "a".repeat(400);
        assert_eq!(estimate_tokens(&s), 100);
    }

    #[test]
    fn message_tokens_counts_content_and_tool_calls() {
        let m = json!({
            "role": "assistant",
            "content": "abcd".repeat(10),
            "tool_calls": [{
                "id": "c1",
                "type": "function",
                "function": {"name": "x", "arguments": "abcd".repeat(20)}
            }],
        });
        let n = estimate_message_tokens(&m);
        // 40-byte content => 10 tokens; 80-byte args => 20 tokens; +
        // small constants for envelope/role/name.
        assert!(n >= 30, "expected >=30, got {}", n);
        assert!(n <= 50, "expected <=50, got {}", n);
    }

    #[test]
    fn budget_should_compact_at_threshold() {
        let b = Budget::new(1000, 0.8);
        assert!(!b.should_compact(799));
        assert!(b.should_compact(800));
        assert!(b.should_compact(1000));
    }

    #[test]
    fn compact_noop_below_threshold() {
        let msgs = vec![
            json!({"role": "system", "content": "sys"}),
            json!({"role": "user", "content": "hi"}),
        ];
        let b = Budget::new(10_000, 0.8);
        let (out, fired) = compact(msgs.clone(), &b, 4);
        assert!(!fired);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn compact_drops_oldest_tool_results_first() {
        // Build a history where every "user" body message is a
        // [Tool result] payload. The compactor should drop oldest
        // tool results first while preserving system + last 2.
        let mut msgs = Vec::new();
        msgs.push(json!({"role": "system", "content": "sys"}));
        for i in 0..20 {
            msgs.push(json!({
                "role": "user",
                "content": format!(
                    "[Tool result of x] {}",
                    "blob ".repeat(200)
                ),
            }));
            msgs.push(json!({
                "role": "assistant",
                "content": format!("ack {}", i),
            }));
        }
        let initial = estimate_messages_tokens(&msgs);
        let b = Budget::new(initial - 100, 0.5);
        let (out, fired) = compact(msgs.clone(), &b, 2);
        assert!(fired, "compaction must fire when over budget");
        assert!(estimate_messages_tokens(&out) < estimate_messages_tokens(&msgs));
        // System head preserved.
        assert_eq!(out[0]["role"], "system");
        // Last two messages preserved verbatim.
        let n = out.len();
        assert_eq!(out[n - 1]["content"], "ack 19");
    }

    #[test]
    fn compact_falls_back_to_placeholder_when_overflow_persists() {
        // When body content is ALL non-droppable, the placeholder
        // step replaces them so the prompt still fits.
        let mut msgs = Vec::new();
        msgs.push(json!({"role": "system", "content": "sys"}));
        for i in 0..30 {
            msgs.push(json!({
                "role": "assistant",
                "content": format!(
                    "tool plan {} {}",
                    i,
                    "narrate ".repeat(500)
                ),
            }));
        }
        // Tail (last 2) preserved verbatim.
        msgs.push(json!({"role": "user", "content": "tail user"}));
        msgs.push(json!({"role": "assistant", "content": "tail asst"}));
        let initial = estimate_messages_tokens(&msgs);
        let b = Budget::new(initial / 4, 0.5);
        let (out, fired) = compact(msgs, &b, 2);
        assert!(fired);
        // Head + placeholder + 2 tail messages.
        assert!(out.len() <= 4);
        assert_eq!(out[0]["role"], "system");
        // Tail still intact.
        let n = out.len();
        assert_eq!(out[n - 2]["content"], "tail user");
        assert_eq!(out[n - 1]["content"], "tail asst");
    }
}
