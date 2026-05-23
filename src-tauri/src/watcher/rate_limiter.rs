//! Per-agent token-bucket rate limiter.
//!
//! `capacity` tokens, refilled at a constant rate per minute. `try_acquire`
//! returns `true` only when a token is available; otherwise the caller is
//! expected to drop the chunk (the Watcher does NOT queue indefinitely).
//!
//! Thread-safe via `parking_lot::Mutex`. Sub-millisecond critical section so
//! contention on the hot path is fine.

use parking_lot::Mutex;
use std::time::{Duration, Instant};

/// Token-bucket state for a single agent.
///
/// `capacity == rpm` (the configured calls/minute ceiling). Tokens refill
/// linearly over a minute; a freshly constructed bucket starts full so the
/// first chunk always passes.
#[derive(Debug)]
pub struct TokenBucket {
    capacity: f64,
    /// Tokens added per second (= rpm / 60).
    refill_per_sec: f64,
    inner: Mutex<BucketInner>,
}

#[derive(Debug)]
struct BucketInner {
    tokens: f64,
    last_refill: Instant,
    /// Counter of chunks dropped because the bucket was empty.
    dropped: u64,
}

impl TokenBucket {
    /// Build a new bucket sized to `rpm` calls per minute. `rpm == 0` is
    /// treated as `1` so the bucket can never starve forever.
    pub fn new(rpm: u32) -> Self {
        let cap = rpm.max(1) as f64;
        Self {
            capacity: cap,
            refill_per_sec: cap / 60.0,
            inner: Mutex::new(BucketInner {
                tokens: cap,
                last_refill: Instant::now(),
                dropped: 0,
            }),
        }
    }

    fn refill(&self, inner: &mut BucketInner, now: Instant) {
        let elapsed = now.duration_since(inner.last_refill).as_secs_f64();
        if elapsed > 0.0 {
            inner.tokens = (inner.tokens + elapsed * self.refill_per_sec).min(self.capacity);
            inner.last_refill = now;
        }
    }

    /// Try to consume one token. Returns `true` on success, `false` if the
    /// bucket was empty (and increments the drop counter).
    pub fn try_acquire(&self) -> bool {
        let mut inner = self.inner.lock();
        let now = Instant::now();
        self.refill(&mut inner, now);
        if inner.tokens >= 1.0 {
            inner.tokens -= 1.0;
            true
        } else {
            inner.dropped += 1;
            false
        }
    }

    /// Total chunks dropped (no token available) since construction.
    pub fn dropped(&self) -> u64 {
        self.inner.lock().dropped
    }

    /// Time until the next token will be available, or [`Duration::ZERO`] if
    /// one is already available. Used for `watcher_status.blocked_until`.
    pub fn blocked_for(&self) -> Duration {
        let mut inner = self.inner.lock();
        let now = Instant::now();
        self.refill(&mut inner, now);
        if inner.tokens >= 1.0 {
            return Duration::ZERO;
        }
        let need = 1.0 - inner.tokens;
        let secs = need / self.refill_per_sec;
        Duration::from_secs_f64(secs.max(0.0))
    }

    /// Approximate "calls in the last minute" — `capacity - tokens` rounded
    /// to nearest integer. Cheap to read for the `watcher_status` tool.
    pub fn calls_in_window(&self) -> u32 {
        let mut inner = self.inner.lock();
        let now = Instant::now();
        self.refill(&mut inner, now);
        (self.capacity - inner.tokens).max(0.0).round() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[test]
    fn fresh_bucket_is_full() {
        let b = TokenBucket::new(10);
        assert_eq!(b.calls_in_window(), 0);
        assert!(b.try_acquire());
    }

    #[test]
    fn drops_when_empty() {
        let b = TokenBucket::new(2);
        assert!(b.try_acquire());
        assert!(b.try_acquire());
        // Bucket empty — third call should drop.
        assert!(!b.try_acquire());
        assert_eq!(b.dropped(), 1);
    }

    #[test]
    fn blocked_for_returns_nonzero_when_empty() {
        let b = TokenBucket::new(60); // 1 token/sec
                                      // Drain.
        for _ in 0..60 {
            let _ = b.try_acquire();
        }
        let wait = b.blocked_for();
        // ~1s until next token.
        assert!(wait <= Duration::from_millis(1100));
    }

    #[test]
    fn refills_over_time() {
        // 600 rpm => 10 tokens/sec — easy to test in 200ms.
        let b = TokenBucket::new(600);
        for _ in 0..600 {
            let _ = b.try_acquire();
        }
        assert!(!b.try_acquire());
        sleep(Duration::from_millis(250));
        // ~2.5 tokens refilled — at least one should be acquirable.
        assert!(b.try_acquire());
    }

    #[test]
    fn rpm_zero_is_clamped() {
        // Must not divide-by-zero or starve forever.
        let b = TokenBucket::new(0);
        assert!(b.try_acquire());
    }
}
