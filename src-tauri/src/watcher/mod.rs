//! Watcher — Gemini-backed supervisor that classifies every agent stdout
//! chunk and escalates decision-requests to the Coordinator mailbox.
//!
//! Lives behind the `watcher` feature flag. When enabled, [`Watcher::spawn`]
//! subscribes to the in-process Tauri stdout stream (`EV_AGENT_STDOUT`,
//! emitted by [`crate::agent::AgentManager`]), pipes each chunk through a
//! Gemini classifier (`gemma-3-4b-it`) under a per-agent token-bucket rate
//! limit, and on `decision_request` writes a mail to `role:coordinator` on
//! thread `watcher:<agent_id>`.
//!
//! A separate poll task drains replies on the same thread and injects them
//! back into the originating agent via [`crate::agent::AgentManager::write`].
//!
//! See `README.md` (section "Watcher") for setup and the `GEMINI_API_KEY`
//! contract.

pub mod classifier;
pub mod rate_limiter;
pub mod supervisor;

pub use classifier::{
    classify_chunk, parse_classification, Classification, ClassifierKind, GeminiClient,
};
pub use rate_limiter::TokenBucket;
pub use supervisor::{AgentWatcherStats, Watcher, WatcherStatus};
