//! Single-consumer worker that drains the chat queue.
//!
//! Lives for the lifetime of the app, owns one `tokio::sync::Notify` for
//! wakeups, and calls `Orchestrator::run_chat(text)` for each claimed item.
//! The orchestrator itself is unaware of the queue — it just keeps doing its
//! one-shot turn API.
//!
//! Wakeups: `send_chat` enqueues a row and pings the Notify; the worker
//! drains everything queued for the current session, then parks. After each
//! turn we also re-emit a queue snapshot to the UI so the badge updates as
//! items move from `queued` -> `processing` -> gone.

use crate::chat_queue;
use crate::chat_sessions;
use crate::db::{spawn_blocking, DbPool};
use crate::error::Result;
use crate::orchestrator::Orchestrator;
use serde_json::json;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::sync::Notify;

pub struct ChatQueueWorker {
    db: DbPool,
    orch: Arc<Orchestrator>,
    notify: Arc<Notify>,
    app: parking_lot::RwLock<Option<AppHandle>>,
}

impl ChatQueueWorker {
    pub fn new(db: DbPool, orch: Arc<Orchestrator>) -> Arc<Self> {
        Arc::new(Self {
            db,
            orch,
            notify: Arc::new(Notify::new()),
            app: parking_lot::RwLock::new(None),
        })
    }

    pub fn set_app_handle(&self, app: AppHandle) {
        *self.app.write() = Some(app);
    }

    /// Wake the worker. Cheap; multiple wakes coalesce.
    pub fn poke(&self) {
        self.notify.notify_one();
    }

    /// Snapshot the current session's queue and emit to the UI. Runs the
    /// rusqlite reads on the blocking pool — the previous inline version
    /// froze the orchestrator for 1–10 ms per snapshot under load.
    pub fn emit_snapshot(&self) {
        let db = self.db.clone();
        let app = self.app.read().clone();
        // Best-effort: any DB failure is logged and dropped so a slow /
        // contended read never blocks the orchestrator's hot path.
        tauri::async_runtime::spawn(async move {
            let snapshot = match spawn_blocking(
                move || -> Result<Option<(String, Vec<chat_queue::QueueItem>, i64)>> {
                    let session_id = match chat_sessions::ensure_current(&db) {
                        Ok(id) => id,
                        Err(_) => return Ok(None),
                    };
                    let items = chat_queue::list(&db, &session_id).unwrap_or_default();
                    let pending = chat_queue::pending_count(&db, &session_id).unwrap_or(0);
                    Ok(Some((session_id, items, pending)))
                },
            )
            .await
            {
                Ok(Some(s)) => s,
                _ => return,
            };
            if let Some(app) = app.as_ref() {
                let (session_id, items, pending) = snapshot;
                let _ = app.emit(
                    chat_queue::QUEUE_EVENT,
                    json!({
                        "session_id": session_id,
                        "items": items,
                        "pending": pending,
                    }),
                );
            }
        });
    }

    /// Spawn the long-lived worker task. Returns the handle (we don't keep
    /// it; the task runs forever).
    pub fn spawn(self: Arc<Self>) {
        // On startup, recover any rows the previous run left in `processing`.
        if let Err(e) = chat_queue::recover_inflight(&self.db) {
            tracing::warn!("chat_queue: recover_inflight: {}", e);
        }
        // Emit an initial snapshot so the UI shows whatever survived.
        self.emit_snapshot();

        let me = self.clone();
        // NB: must be `tauri::async_runtime::spawn`, NOT `tokio::spawn`.
        // Called from the Tauri `setup` callback, which runs on the main
        // thread outside any tokio runtime context.
        tauri::async_runtime::spawn(async move {
            loop {
                if let Err(e) = me.drain_once().await {
                    tracing::error!("chat_queue worker: {}", e);
                    if !chat_queue::continue_on_error(&me.db) {
                        // Stay parked until the next poke. Don't try to
                        // claim further work.
                        me.notify.notified().await;
                        continue;
                    }
                }
                // Park until poked.
                me.notify.notified().await;
            }
        });
    }

    /// Drain the queue for the current session until it's empty. Returns
    /// `Ok(())` even if individual turns fail — error policy is governed by
    /// `continue_on_error`. Bubbles up only infrastructure-level failures
    /// (DB unavailable, etc).
    ///
    /// Every SQLite call is wrapped in `spawn_blocking` — leaving the
    /// rusqlite calls inline here would park the entire tokio worker
    /// thread on disk I/O, blocking the orchestrator's LLM call AND any
    /// unrelated Tauri command in the meantime. `claim_next` and
    /// `ensure_current` share one blocking call because they both touch
    /// the same short transaction inside `claim_next`.
    async fn drain_once(&self) -> Result<()> {
        loop {
            let db = self.db.clone();
            let claimed = spawn_blocking(move || -> Result<Option<chat_queue::QueueItem>> {
                let session_id = chat_sessions::ensure_current(&db)?;
                chat_queue::claim_next(&db, &session_id)
            })
            .await?;
            let item = match claimed {
                Some(it) => it,
                None => return Ok(()),
            };
            // The queue snapshot now shows item.status='processing'.
            self.emit_snapshot();

            let res = self.orch.clone().run_chat(item.text.clone()).await;
            match res {
                Ok(()) => {
                    let db = self.db.clone();
                    let id = item.id.clone();
                    let _ = spawn_blocking(move || chat_queue::mark_done(&db, &id)).await;
                }
                Err(e) => {
                    tracing::error!("orchestrator turn failed: {}", e);
                    let db = self.db.clone();
                    let id = item.id.clone();
                    let _ = spawn_blocking(move || chat_queue::mark_failed(&db, &id)).await;
                    if !chat_queue::continue_on_error(&self.db) {
                        self.emit_snapshot();
                        return Ok(());
                    }
                }
            }
            self.emit_snapshot();
        }
    }
}
