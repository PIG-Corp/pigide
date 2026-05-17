//! Watcher supervisor — wires the agent stdout stream to the Gemini
//! classifier, the per-agent rate limiter, the Coordinator mailbox, and the
//! reply-injection loop.
//!
//! Listens on the in-process Tauri event channel `EV_AGENT_STDOUT` (already
//! emitted by [`crate::agent::AgentManager`] for every PTY chunk) instead of
//! polling log files. The chunk path therefore stays "real-time" — the same
//! event the frontend xterm sees.
//!
//! Replies from the Coordinator are picked up by polling the mailbox for
//! mail addressed to `watcher:<agent_id>` thread; once a reply arrives it is
//! marked read and written into the agent's stdin via
//! [`crate::agent::AgentManager::write`].

use crate::agent::AgentManager;
use crate::db::DbPool;
use crate::events::EV_AGENT_STDOUT;
use crate::swarm::mailbox;
use crate::watcher::classifier::{
    classify_chunk, ClassifierKind, GeminiClient,
};
use crate::watcher::rate_limiter::TokenBucket;
use base64::Engine;
use parking_lot::RwLock;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Listener};

/// Default per-agent calls/minute. Override via `PIGIDE_WATCHER_RPM`.
const DEFAULT_RPM: u32 = 10;

/// How often the reply pump drains the mailbox, in milliseconds.
const REPLY_POLL_MS: u64 = 1500;

/// Per-agent runtime stats reported by the `watcher_status` tool.
#[derive(Debug, Clone, Serialize)]
pub struct AgentWatcherStats {
    /// `decision_request` | `noise` | `none` — last classification verdict.
    pub last_classification: String,
    /// Calls consumed in the rolling 1-minute window.
    pub calls_this_minute: u32,
    /// ISO-8601 timestamp at which the bucket will refill enough for the
    /// next chunk, or `None` when the bucket is non-empty right now.
    pub blocked_until: Option<String>,
    /// Total chunks dropped because the bucket was empty.
    pub dropped: u64,
}

/// Aggregate snapshot for the `watcher_status` tool.
#[derive(Debug, Clone, Serialize)]
pub struct WatcherStatus {
    pub enabled: bool,
    pub rpm: u32,
    pub agents: HashMap<String, AgentWatcherStats>,
}

#[derive(Default)]
struct WatcherInner {
    /// Per-agent token buckets. Created lazily on first chunk.
    buckets: HashMap<String, Arc<TokenBucket>>,
    /// Last classification per agent — populated by the classifier task.
    last: HashMap<String, String>,
    /// Open decision-request threads waiting for a Coordinator reply.
    open_threads: HashMap<String, Instant>,
}

/// The Watcher singleton. Cheap to clone (`Arc` inside).
pub struct Watcher {
    db: DbPool,
    agent_mgr: Arc<AgentManager>,
    client: GeminiClient,
    rpm: u32,
    inner: Arc<RwLock<WatcherInner>>,
}

impl Watcher {
    /// Construct a new Watcher. Reads `GEMINI_API_KEY` and
    /// `PIGIDE_WATCHER_RPM` from the environment. Returns `Err` (and the
    /// caller logs+disables) when the API key is missing.
    pub fn new(db: DbPool, agent_mgr: Arc<AgentManager>) -> Result<Self, String> {
        let client = GeminiClient::from_env()?;
        let rpm = std::env::var("PIGIDE_WATCHER_RPM")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_RPM);
        Ok(Self {
            db,
            agent_mgr,
            client,
            rpm,
            inner: Arc::new(RwLock::new(WatcherInner::default())),
        })
    }

    /// Override the Gemini client (test hook — points at a `wiremock` mock).
    pub fn with_client(mut self, client: GeminiClient) -> Self {
        self.client = client;
        self
    }

    /// Override the RPM (test hook).
    pub fn with_rpm(mut self, rpm: u32) -> Self {
        self.rpm = rpm.max(1);
        self
    }

    /// Look up (or lazily create) the bucket for an agent.
    fn bucket(&self, agent_id: &str) -> Arc<TokenBucket> {
        if let Some(b) = self.inner.read().buckets.get(agent_id).cloned() {
            return b;
        }
        let b = Arc::new(TokenBucket::new(self.rpm));
        self.inner
            .write()
            .buckets
            .insert(agent_id.to_string(), b.clone());
        b
    }

    fn record_classification(&self, agent_id: &str, kind: ClassifierKind) {
        self.inner
            .write()
            .last
            .insert(agent_id.to_string(), kind.as_str().to_string());
    }

    fn note_open_thread(&self, agent_id: &str) {
        self.inner
            .write()
            .open_threads
            .insert(agent_id.to_string(), Instant::now());
    }

    fn close_thread(&self, agent_id: &str) {
        self.inner.write().open_threads.remove(agent_id);
    }

    /// Snapshot for the `watcher_status` MCP tool.
    pub fn status(&self) -> WatcherStatus {
        let inner = self.inner.read();
        let mut agents = HashMap::with_capacity(inner.buckets.len());
        for (id, bucket) in inner.buckets.iter() {
            let blocked = bucket.blocked_for();
            let blocked_until = if blocked.is_zero() {
                None
            } else {
                let when = chrono::Utc::now()
                    + chrono::Duration::milliseconds(blocked.as_millis() as i64);
                Some(when.to_rfc3339())
            };
            agents.insert(
                id.clone(),
                AgentWatcherStats {
                    last_classification: inner
                        .last
                        .get(id)
                        .cloned()
                        .unwrap_or_else(|| "none".to_string()),
                    calls_this_minute: bucket.calls_in_window(),
                    blocked_until,
                    dropped: bucket.dropped(),
                },
            );
        }
        WatcherStatus {
            enabled: true,
            rpm: self.rpm,
            agents,
        }
    }

    /// Spawn the listener + reply-pump tasks. Idempotent at the call site —
    /// Tauri's `listen` allocates a fresh handler per call, so callers must
    /// invoke this exactly once per app boot.
    pub fn spawn(self: Arc<Self>, app: AppHandle) {
        let me = self.clone();
        let app_for_listener = app.clone();
        // Listener: every EV_AGENT_STDOUT triggers a classification task.
        // Tauri delivers the payload as JSON (`{agent_id, data_b64}`) — see
        // `agent.rs`. We decode the b64, then hand the chunk to a tokio task
        // so the listener thread is never blocked on HTTP.
        app.listen(EV_AGENT_STDOUT, move |event| {
            let payload = event.payload().to_string();
            let me = me.clone();
            tauri::async_runtime::spawn(async move {
                me.handle_stdout_event(&payload).await;
            });
        });
        // Reply pump.
        let me = self.clone();
        let _ = app_for_listener; // unused outside the listener
        tauri::async_runtime::spawn(async move {
            me.run_reply_pump().await;
        });
    }

    async fn handle_stdout_event(&self, payload: &str) {
        #[derive(serde::Deserialize)]
        struct Wire<'a> {
            agent_id: &'a str,
            data_b64: &'a str,
        }
        let wire: Wire = match serde_json::from_str(payload) {
            Ok(w) => w,
            Err(_) => return,
        };
        let bytes = match base64::engine::general_purpose::STANDARD.decode(wire.data_b64) {
            Ok(b) => b,
            Err(_) => return,
        };
        let chunk = String::from_utf8_lossy(&bytes).to_string();
        if chunk.trim().is_empty() {
            return;
        }
        self.process_chunk(wire.agent_id, &chunk).await;
    }

    /// Public for tests — exposes the classify→escalate path without going
    /// through the Tauri event bus.
    pub async fn process_chunk(&self, agent_id: &str, chunk: &str) {
        let bucket = self.bucket(agent_id);
        if !bucket.try_acquire() {
            // Drop on the floor — we never queue indefinitely. The drop
            // counter is incremented by the bucket itself.
            return;
        }
        let verdict = match classify_chunk(&self.client, chunk).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(agent = %agent_id, "watcher classify: {}", e);
                return;
            }
        };
        self.record_classification(agent_id, verdict.kind);
        if verdict.kind != ClassifierKind::DecisionRequest {
            return;
        }
        let body = serde_json::json!({
            "agent_id": agent_id,
            "prompt_text": verdict.prompt_text,
            "options": verdict.options,
        })
        .to_string();
        let thread_id = format!("watcher:{}", agent_id);
        if let Err(e) = mailbox::send(
            &self.db,
            None,
            "role:coordinator",
            &body,
            Some(&thread_id),
        ) {
            tracing::warn!(agent = %agent_id, "watcher escalate mail: {}", e);
            return;
        }
        self.note_open_thread(agent_id);
    }

    async fn run_reply_pump(&self) {
        let mut tick = tokio::time::interval(Duration::from_millis(REPLY_POLL_MS));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            self.drain_replies();
        }
    }

    /// Drain Coordinator replies on every `watcher:<agent_id>` thread that
    /// is currently open. Public for tests.
    pub fn drain_replies(&self) {
        let agents: Vec<String> = self
            .inner
            .read()
            .open_threads
            .keys()
            .cloned()
            .collect();
        for agent_id in agents {
            let thread_id = format!("watcher:{}", agent_id);
            let mails = match mailbox::list_thread(&self.db, &thread_id) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(agent = %agent_id, "watcher list_thread: {}", e);
                    continue;
                }
            };
            // We only want replies that did not originate from the Watcher
            // (Watcher sends with `from_agent_id = None`). Replies arrive
            // from the Coordinator agent and should still be unread.
            let mut to_mark = Vec::new();
            for m in mails {
                if m.read_at.is_some() {
                    continue;
                }
                if m.from_agent_id.is_none() {
                    // Our own escalation mail — skip.
                    continue;
                }
                let payload = build_injection(&m.body);
                if let Err(e) = self.agent_mgr.write(&agent_id, payload.as_bytes()) {
                    tracing::warn!(
                        agent = %agent_id,
                        "watcher inject reply: {}",
                        e
                    );
                    continue;
                }
                to_mark.push(m.id);
                self.close_thread(&agent_id);
            }
            if !to_mark.is_empty() {
                let _ = mailbox::mark_read(&self.db, &to_mark);
            }
        }
    }
}

/// Format a Coordinator reply for injection into the agent's stdin.
///
/// Wraps the body with carriage returns so PTY-attached CLIs treat it as a
/// single line + Enter, matching the `Architect::execute` pattern.
fn build_injection(body: &str) -> String {
    format!("\r{}\r", body.trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watcher::classifier::{Classification, ClassifierKind};

    #[test]
    fn build_injection_wraps_with_cr() {
        let s = build_injection("y");
        assert!(s.starts_with('\r'));
        assert!(s.ends_with('\r'));
        assert!(s.contains("y"));
    }

    #[test]
    fn classification_round_trip_serde() {
        let c = Classification {
            kind: ClassifierKind::DecisionRequest,
            prompt_text: Some("Proceed?".into()),
            options: vec!["yes".into(), "no".into()],
        };
        let s = serde_json::to_string(&c).unwrap();
        let back: Classification = serde_json::from_str(&s).unwrap();
        assert_eq!(back, c);
    }
}
