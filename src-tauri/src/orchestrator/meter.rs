//! Global LLM spend / rate metering.
//!
//! Without a ceiling, a runaway tool-loop, a scripted `send_chat` flood, or a
//! misbehaving agent can drive unbounded provider calls — each chat turn is up
//! to `MAX_ITERATIONS` LLM round-trips, the chat-queue worker drains without
//! pause, and there was previously no global counter tying them together. This
//! module is that counter.
//!
//! It is deliberately a process-global, in-memory rolling-window meter:
//! - **requests/min** and **estimated tokens/min** over a 60s sliding window,
//! - checked *before* each provider call via [`Meter::check_and_reserve`],
//! - caps read from settings (so the operator can tune without a rebuild),
//!   with conservative defaults that never trip in normal interactive use.
//!
//! It is NOT a billing system — the token figure is the cheap `budget`
//! estimator, not a provider-authoritative count. The goal is to convert
//! "silently burn the credit card" into "stop with a clear error after N
//! calls/minute", which the orchestrator surfaces to the user.

use crate::db::{self, DbPool};
use parking_lot::Mutex;
use std::time::{Duration, Instant};

pub const KEY_MAX_REQ_PER_MIN: &str = "llm.max_requests_per_min";
pub const KEY_MAX_TOKENS_PER_MIN: &str = "llm.max_tokens_per_min";

/// Defaults sized so interactive use never trips them, but a runaway loop
/// does within a minute. A single user turn is ≤ MAX_ITERATIONS (6) calls;
/// 120/min allows ~20 turns/min before braking.
pub const DEFAULT_MAX_REQ_PER_MIN: u32 = 120;
/// ~4M est-tokens/min. At the 200k soft budget cap per call that's ~20 full
/// calls/min — again, far above interactive, below runaway.
pub const DEFAULT_MAX_TOKENS_PER_MIN: u64 = 4_000_000;

const WINDOW: Duration = Duration::from_secs(60);

#[derive(Clone, Copy)]
struct Sample {
    at: Instant,
    tokens: u64,
}

struct Inner {
    samples: Vec<Sample>,
}

/// Process-global rolling-window meter.
pub struct Meter {
    inner: Mutex<Inner>,
}

impl Default for Meter {
    fn default() -> Self {
        Self::new()
    }
}

impl Meter {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                samples: Vec::new(),
            }),
        }
    }

    fn prune(inner: &mut Inner, now: Instant) {
        let cutoff = now.checked_sub(WINDOW);
        inner.samples.retain(|s| match cutoff {
            Some(c) => s.at >= c,
            None => true,
        });
    }

    /// Check the caps and, if within budget, record a reservation for one
    /// request of `est_tokens`. Returns `Err(reason)` when a cap would be
    /// exceeded — the caller (orchestrator) turns that into a user-visible
    /// error and stops the turn. Reservation is NOT made when rejected.
    pub fn check_and_reserve(
        &self,
        est_tokens: u64,
        max_req_per_min: u32,
        max_tokens_per_min: u64,
    ) -> Result<(), String> {
        let now = Instant::now();
        let mut inner = self.inner.lock();
        Self::prune(&mut inner, now);

        let req_count = inner.samples.len() as u32;
        let tok_count: u64 = inner.samples.iter().map(|s| s.tokens).sum();

        if req_count >= max_req_per_min {
            return Err(format!(
                "LLM request rate cap reached ({} requests in the last 60s, limit {}). \
                 Stopping to prevent runaway spend. Adjust `{}` in settings to raise it.",
                req_count, max_req_per_min, KEY_MAX_REQ_PER_MIN
            ));
        }
        if tok_count.saturating_add(est_tokens) > max_tokens_per_min {
            return Err(format!(
                "LLM token rate cap reached (~{} est-tokens in the last 60s + {} this call, \
                 limit {}). Stopping to prevent runaway spend. Adjust `{}` in settings.",
                tok_count, est_tokens, max_tokens_per_min, KEY_MAX_TOKENS_PER_MIN
            ));
        }

        inner.samples.push(Sample {
            at: now,
            tokens: est_tokens,
        });
        Ok(())
    }

    /// Current window stats (requests, est-tokens) — for telemetry / UI.
    pub fn stats(&self) -> (u32, u64) {
        let now = Instant::now();
        let mut inner = self.inner.lock();
        Self::prune(&mut inner, now);
        let req = inner.samples.len() as u32;
        let tok = inner.samples.iter().map(|s| s.tokens).sum();
        (req, tok)
    }
}

/// Estimate the token cost of one provider call: messages + tool catalogue,
/// using the cheap `budget` byte-heuristic estimator. Used to reserve against
/// the per-minute token cap before the call goes out.
pub fn estimate_request_tokens(messages: &[serde_json::Value], tools: &[serde_json::Value]) -> u64 {
    let msg_tokens = super::budget::estimate_messages_tokens(messages) as u64;
    let tool_tokens: u64 = tools
        .iter()
        .map(|t| super::budget::estimate_tokens(&t.to_string()) as u64)
        .sum();
    msg_tokens + tool_tokens
}

/// Read the configured caps from settings, falling back to defaults. A value
/// of `0` disables that particular cap (treated as unlimited).
pub fn caps(db: &DbPool) -> (u32, u64) {
    let req = db::get_setting(db, KEY_MAX_REQ_PER_MIN)
        .ok()
        .flatten()
        .and_then(|v| v.parse::<u32>().ok())
        .map(|v| if v == 0 { u32::MAX } else { v })
        .unwrap_or(DEFAULT_MAX_REQ_PER_MIN);
    let tok = db::get_setting(db, KEY_MAX_TOKENS_PER_MIN)
        .ok()
        .flatten()
        .and_then(|v| v.parse::<u64>().ok())
        .map(|v| if v == 0 { u64::MAX } else { v })
        .unwrap_or(DEFAULT_MAX_TOKENS_PER_MIN);
    (req, tok)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserves_until_request_cap() {
        let m = Meter::new();
        // cap = 3 requests/min, generous token budget
        for _ in 0..3 {
            assert!(m.check_and_reserve(10, 3, u64::MAX).is_ok());
        }
        // 4th must be rejected.
        let err = m.check_and_reserve(10, 3, u64::MAX).unwrap_err();
        assert!(err.contains("request rate cap"));
    }

    #[test]
    fn reserves_until_token_cap() {
        let m = Meter::new();
        // token cap = 100; each call 40 tokens.
        assert!(m.check_and_reserve(40, u32::MAX, 100).is_ok()); // 40
        assert!(m.check_and_reserve(40, u32::MAX, 100).is_ok()); // 80
                                                                 // third would push to 120 > 100 → reject.
        let err = m.check_and_reserve(40, u32::MAX, 100).unwrap_err();
        assert!(err.contains("token rate cap"));
    }

    #[test]
    fn rejected_call_does_not_reserve() {
        let m = Meter::new();
        // Immediately over the token cap.
        assert!(m.check_and_reserve(200, u32::MAX, 100).is_err());
        let (req, tok) = m.stats();
        assert_eq!(req, 0);
        assert_eq!(tok, 0);
    }

    #[test]
    fn zero_cap_means_unlimited_via_caps_helper() {
        // The caps() helper maps 0 → MAX; verify the meter never trips then.
        let m = Meter::new();
        for _ in 0..1000 {
            assert!(m.check_and_reserve(1_000_000, u32::MAX, u64::MAX).is_ok());
        }
    }

    #[test]
    fn stats_reports_window_usage() {
        let m = Meter::new();
        m.check_and_reserve(10, u32::MAX, u64::MAX).unwrap();
        m.check_and_reserve(20, u32::MAX, u64::MAX).unwrap();
        let (req, tok) = m.stats();
        assert_eq!(req, 2);
        assert_eq!(tok, 30);
    }
}
