//! Chat message queue.
//!
//! Pipeline: `send_chat` enqueues a `QueueItem` (status = `queued`), the
//! single-consumer worker picks the head, marks it `processing`, runs the
//! orchestrator turn for it, then loops to the next. State is persisted to
//! the `chat_queue` table so a crash / restart preserves user intent (any
//! `processing` row found at startup is rolled back to `queued`).
//!
//! Single consumer = no race: only one turn is in flight at any time. The
//! orchestrator itself never sees the queue — it just keeps its existing
//! `run_chat(text)` API; the worker is the one calling it.

use crate::db::DbPool;
use crate::error::{Error, Result};
use crate::events::EV_CHAT_QUEUE;
use crate::path_suggest::Attachment;
use chrono::Utc;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Status of a queued message.
///
/// Lifecycle: `queued` -> `processing` -> (deleted on success or `failed`).
/// `cancelled` is a terminal state for items the user nuked while still
/// in the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QueueStatus {
    Queued,
    Processing,
    Failed,
    Cancelled,
}

impl QueueStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            QueueStatus::Queued => "queued",
            QueueStatus::Processing => "processing",
            QueueStatus::Failed => "failed",
            QueueStatus::Cancelled => "cancelled",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "queued" => Some(QueueStatus::Queued),
            "processing" => Some(QueueStatus::Processing),
            "failed" => Some(QueueStatus::Failed),
            "cancelled" => Some(QueueStatus::Cancelled),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueItem {
    pub id: String,
    pub session_id: String,
    pub text: String,
    pub status: String, // serialise as string for the frontend
    pub position: i64,
    pub created_at: String,
    /// Validated `@`-mention attachments for this message. Empty vec when
    /// the user sent a plain text message. Surfaces in the orchestrator's
    /// `[WORLD STATE]` block for the turn that consumes this row.
    #[serde(default)]
    pub attachments: Vec<Attachment>,
}

/// Idempotent table creation. Called from `db::init_pool` migrations, but
/// we also call it on first use as a defence-in-depth net (matches the
/// `skills::trace::ensure_table` pattern already in the codebase).
pub fn ensure_table(db: &DbPool) -> Result<()> {
    let conn = db.get()?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS chat_queue (
            id          TEXT PRIMARY KEY,
            session_id  TEXT NOT NULL,
            text        TEXT NOT NULL,
            status      TEXT NOT NULL DEFAULT 'queued',
            position    INTEGER NOT NULL,
            created_at  TEXT NOT NULL,
            attachments_json TEXT
         );
         CREATE INDEX IF NOT EXISTS idx_chat_queue_session_pos
            ON chat_queue(session_id, position);
         CREATE INDEX IF NOT EXISTS idx_chat_queue_status
            ON chat_queue(status);",
    )?;
    // Defence-in-depth: if the table existed before v13, ALTER it now.
    // SQLite is happy to no-op an "already exists" column add; we ignore
    // the error so this stays idempotent.
    let _ = conn.execute_batch("ALTER TABLE chat_queue ADD COLUMN attachments_json TEXT");
    Ok(())
}

/// On startup, recover any `processing` rows back to `queued` so the worker
/// will pick them up again. The orchestrator's own `delete_after` already
/// rolled back partial assistant/tool messages on the previous error path,
/// so re-running the user message is safe.
pub fn recover_inflight(db: &DbPool) -> Result<usize> {
    let conn = db.get()?;
    let n = conn.execute(
        "UPDATE chat_queue SET status='queued' WHERE status='processing'",
        [],
    )?;
    Ok(n)
}

/// Append a new user message to the tail of the queue for `session_id`.
/// Returns the inserted row.
///
/// Empty / whitespace-only text is rejected. Consecutive duplicates inside
/// the still-pending tail are also rejected — guards against double-tap on
/// Enter or stray accessibility events that would otherwise spam the model.
pub fn enqueue(db: &DbPool, session_id: &str, text: &str) -> Result<QueueItem> {
    enqueue_with_attachments(db, session_id, text, Vec::new())
}

/// Append a new user message with validated attachments. Same dedupe /
/// empty-text rules as [`enqueue`]. The attachments are persisted as JSON
/// in `chat_queue.attachments_json` and replayed by the worker.
pub fn enqueue_with_attachments(
    db: &DbPool,
    session_id: &str,
    text: &str,
    attachments: Vec<Attachment>,
) -> Result<QueueItem> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(Error::Invalid("empty message".into()));
    }
    let conn = db.get()?;
    // Dedupe: if the LATEST not-yet-finished row for this session has the
    // same text, skip.
    let last: Option<(String, String)> = conn
        .query_row(
            "SELECT text, status FROM chat_queue
             WHERE session_id=?1 AND status IN ('queued','processing')
             ORDER BY position DESC LIMIT 1",
            [session_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .ok();
    if let Some((prev_text, _)) = last {
        if prev_text == trimmed {
            return Err(Error::Invalid("duplicate of previous message".into()));
        }
    }
    let next_pos: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(position), 0) + 1 FROM chat_queue WHERE session_id=?1",
            [session_id],
            |r| r.get(0),
        )
        .unwrap_or(1);
    let attachments_json: Option<String> = if attachments.is_empty() {
        None
    } else {
        Some(serde_json::to_string(&attachments)?)
    };
    let item = QueueItem {
        id: Uuid::new_v4().to_string(),
        session_id: session_id.to_string(),
        text: trimmed.to_string(),
        status: QueueStatus::Queued.as_str().into(),
        position: next_pos,
        created_at: Utc::now().to_rfc3339(),
        attachments,
    };
    conn.execute(
        "INSERT INTO chat_queue(id,session_id,text,status,position,created_at,attachments_json)
         VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![
            &item.id,
            &item.session_id,
            &item.text,
            &item.status,
            item.position,
            &item.created_at,
            &attachments_json,
        ],
    )?;
    Ok(item)
}

/// Return the head item still in `queued` for this session, atomically
/// flipping it to `processing`. Returns `None` if the queue is empty.
///
/// Single-consumer guarantee: only the worker calls this, so SELECT-then-UPDATE
/// is safe; we still use a transaction so a concurrent `cancel` cannot race
/// us into a half-state.
pub fn claim_next(db: &DbPool, session_id: &str) -> Result<Option<QueueItem>> {
    let mut conn = db.get()?;
    let tx = conn.transaction()?;
    let row: Option<QueueItem> = tx
        .query_row(
            "SELECT id,session_id,text,status,position,created_at,attachments_json
             FROM chat_queue
             WHERE session_id=?1 AND status='queued'
             ORDER BY position ASC LIMIT 1",
            [session_id],
            |r| {
                let attachments_json: Option<String> = r.get(6)?;
                let attachments = attachments_json
                    .and_then(|s| serde_json::from_str::<Vec<Attachment>>(&s).ok())
                    .unwrap_or_default();
                Ok(QueueItem {
                    id: r.get(0)?,
                    session_id: r.get(1)?,
                    text: r.get(2)?,
                    status: r.get(3)?,
                    position: r.get(4)?,
                    created_at: r.get(5)?,
                    attachments,
                })
            },
        )
        .ok();
    let mut item = match row {
        Some(it) => it,
        None => {
            tx.commit()?;
            return Ok(None);
        }
    };
    tx.execute(
        "UPDATE chat_queue SET status='processing' WHERE id=?1 AND status='queued'",
        [&item.id],
    )?;
    tx.commit()?;
    item.status = QueueStatus::Processing.as_str().into();
    Ok(Some(item))
}

/// Remove a queued item (only if still `queued` — items that have already
/// flipped to `processing` cannot be cancelled mid-turn). Returns `true` if
/// a row was actually removed.
pub fn cancel(db: &DbPool, id: &str) -> Result<bool> {
    let conn = db.get()?;
    let n = conn.execute(
        "DELETE FROM chat_queue WHERE id=?1 AND status='queued'",
        [id],
    )?;
    Ok(n > 0)
}

/// Mark item as done. The row is removed from the queue (we keep failures
/// around briefly via `mark_failed`, see below).
pub fn mark_done(db: &DbPool, id: &str) -> Result<()> {
    let conn = db.get()?;
    conn.execute("DELETE FROM chat_queue WHERE id=?1", [id])?;
    Ok(())
}

/// Flip `processing` row to `failed`. The orchestrator already inserted a
/// system error message into the chat itself, so we just remove the row to
/// keep the queue clean. We expose the function as `mark_failed` for
/// symmetry / future telemetry.
pub fn mark_failed(db: &DbPool, id: &str) -> Result<()> {
    let conn = db.get()?;
    conn.execute("DELETE FROM chat_queue WHERE id=?1", [id])?;
    Ok(())
}

/// Snapshot of all not-yet-finished items in this session, in send order.
pub fn list(db: &DbPool, session_id: &str) -> Result<Vec<QueueItem>> {
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT id,session_id,text,status,position,created_at,attachments_json
         FROM chat_queue
         WHERE session_id=?1 AND status IN ('queued','processing')
         ORDER BY position ASC",
    )?;
    let rows = stmt.query_map([session_id], |r| {
        let attachments_json: Option<String> = r.get(6)?;
        let attachments = attachments_json
            .and_then(|s| serde_json::from_str::<Vec<Attachment>>(&s).ok())
            .unwrap_or_default();
        Ok(QueueItem {
            id: r.get(0)?,
            session_id: r.get(1)?,
            text: r.get(2)?,
            status: r.get(3)?,
            position: r.get(4)?,
            created_at: r.get(5)?,
            attachments,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Number of items currently `queued` for this session (excludes the one
/// being processed). Used by the UI badge.
pub fn pending_count(db: &DbPool, session_id: &str) -> Result<i64> {
    let conn = db.get()?;
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM chat_queue WHERE session_id=?1 AND status='queued'",
        [session_id],
        |r| r.get(0),
    )?;
    Ok(n)
}

/// Read `chat.queue.continue_on_error` from settings. Default = true.
pub fn continue_on_error(db: &DbPool) -> bool {
    crate::db::get_setting(db, "chat.queue.continue_on_error")
        .ok()
        .flatten()
        .map(|v| !v.eq_ignore_ascii_case("false"))
        .unwrap_or(true)
}

/// Channel name for queue snapshot updates.
pub const QUEUE_EVENT: &str = EV_CHAT_QUEUE;

#[cfg(test)]
mod tests {
    use super::*;
    use r2d2::Pool;
    use r2d2_sqlite::SqliteConnectionManager;

    fn test_pool() -> DbPool {
        let manager = SqliteConnectionManager::memory();
        let pool = Pool::builder().max_size(2).build(manager).unwrap();
        // settings table is needed for `continue_on_error` reads in some tests.
        {
            let conn = pool.get().unwrap();
            conn.execute_batch("CREATE TABLE settings(key TEXT PRIMARY KEY, value TEXT NOT NULL);")
                .unwrap();
        }
        ensure_table(&pool).unwrap();
        pool
    }

    #[test]
    fn enqueue_rejects_empty() {
        let p = test_pool();
        assert!(enqueue(&p, "s1", "").is_err());
        assert!(enqueue(&p, "s1", "   ").is_err());
        assert_eq!(pending_count(&p, "s1").unwrap(), 0);
    }

    #[test]
    fn enqueue_rejects_consecutive_duplicate() {
        let p = test_pool();
        enqueue(&p, "s1", "hello").unwrap();
        let err = enqueue(&p, "s1", "hello").err().unwrap();
        assert!(err.to_string().contains("duplicate"));
        // Different text is fine.
        enqueue(&p, "s1", "world").unwrap();
        // Same text again now is fine because it's not the latest.
        // Wait — it IS the latest now, so it should still dedupe. Let's keep
        // semantics tight: we check ONLY the most recent pending row.
        assert!(enqueue(&p, "s1", "world").is_err());
        assert!(enqueue(&p, "s1", "hello").is_ok());
    }

    #[test]
    fn claim_next_walks_queue_in_order() {
        let p = test_pool();
        let a = enqueue(&p, "s1", "msg1").unwrap();
        let b = enqueue(&p, "s1", "msg2").unwrap();
        let c = enqueue(&p, "s1", "msg3").unwrap();

        let head = claim_next(&p, "s1").unwrap().unwrap();
        assert_eq!(head.id, a.id);
        assert_eq!(head.status, "processing");

        // Cannot double-claim while head is processing — next should be msg2.
        mark_done(&p, &head.id).unwrap();
        let head2 = claim_next(&p, "s1").unwrap().unwrap();
        assert_eq!(head2.id, b.id);

        mark_done(&p, &head2.id).unwrap();
        let head3 = claim_next(&p, "s1").unwrap().unwrap();
        assert_eq!(head3.id, c.id);
    }

    #[test]
    fn cancel_drops_queued_skips_processing() {
        let p = test_pool();
        let a = enqueue(&p, "s1", "first").unwrap();
        let b = enqueue(&p, "s1", "second").unwrap();
        let c = enqueue(&p, "s1", "third").unwrap();

        let head = claim_next(&p, "s1").unwrap().unwrap();
        assert_eq!(head.id, a.id);

        // Cancel msg2 while msg1 is processing.
        assert!(cancel(&p, &b.id).unwrap());

        // Cancelling the in-flight head returns false (not allowed).
        assert!(!cancel(&p, &a.id).unwrap());

        // After msg1 finishes, next is msg3 — msg2 is gone.
        mark_done(&p, &a.id).unwrap();
        let head2 = claim_next(&p, "s1").unwrap().unwrap();
        assert_eq!(head2.id, c.id);
    }

    #[test]
    fn pending_count_excludes_processing() {
        let p = test_pool();
        enqueue(&p, "s1", "a").unwrap();
        enqueue(&p, "s1", "b").unwrap();
        enqueue(&p, "s1", "c").unwrap();
        assert_eq!(pending_count(&p, "s1").unwrap(), 3);
        let _ = claim_next(&p, "s1").unwrap().unwrap();
        // One in flight, two still queued.
        assert_eq!(pending_count(&p, "s1").unwrap(), 2);
    }

    #[test]
    fn recover_inflight_resets_processing() {
        let p = test_pool();
        let a = enqueue(&p, "s1", "boom").unwrap();
        let _ = claim_next(&p, "s1").unwrap().unwrap();
        // Pretend the app crashed. Recovery flips it back to queued.
        let n = recover_inflight(&p).unwrap();
        assert_eq!(n, 1);
        let head = claim_next(&p, "s1").unwrap().unwrap();
        assert_eq!(head.id, a.id);
        assert_eq!(head.status, "processing");
    }

    #[test]
    fn list_returns_full_pending_snapshot() {
        let p = test_pool();
        enqueue(&p, "s1", "a").unwrap();
        enqueue(&p, "s1", "b").unwrap();
        enqueue(&p, "s2", "ignored").unwrap();
        let items = list(&p, "s1").unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].text, "a");
        assert_eq!(items[1].text, "b");
        assert_eq!(items[0].status, "queued");
    }

    #[test]
    fn continue_on_error_default_is_true() {
        let p = test_pool();
        assert!(continue_on_error(&p));
        crate::db::set_setting(&p, "chat.queue.continue_on_error", "false").unwrap();
        assert!(!continue_on_error(&p));
        crate::db::set_setting(&p, "chat.queue.continue_on_error", "true").unwrap();
        assert!(continue_on_error(&p));
    }

    #[test]
    fn enqueue_isolates_sessions() {
        let p = test_pool();
        enqueue(&p, "s1", "a").unwrap();
        enqueue(&p, "s2", "b").unwrap();
        assert_eq!(pending_count(&p, "s1").unwrap(), 1);
        assert_eq!(pending_count(&p, "s2").unwrap(), 1);
        let head1 = claim_next(&p, "s1").unwrap().unwrap();
        assert_eq!(head1.text, "a");
        // s2 untouched.
        assert_eq!(pending_count(&p, "s2").unwrap(), 1);
    }

    #[test]
    fn enqueue_with_attachments_round_trips_via_claim_next() {
        let p = test_pool();
        let attachments = vec![Attachment {
            kind: "file".into(),
            path: "/abs/main.rs".into(),
            label: "src/main.rs".into(),
        }];
        enqueue_with_attachments(&p, "s", "look at this", attachments.clone()).unwrap();
        let head = claim_next(&p, "s").unwrap().unwrap();
        assert_eq!(head.attachments, attachments);
    }

    #[test]
    fn list_returns_attachments_for_pending_rows() {
        let p = test_pool();
        let a = vec![Attachment {
            kind: "dir".into(),
            path: "/abs/dir".into(),
            label: "dir/".into(),
        }];
        enqueue_with_attachments(&p, "s", "msg", a.clone()).unwrap();
        let items = list(&p, "s").unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].attachments, a);
    }

    #[test]
    fn enqueue_without_attachments_persists_null_column() {
        // Backwards-compat: rows that came from before v13 had NULL
        // attachments_json. Re-load must surface them as `vec![]` rather
        // than failing.
        let p = test_pool();
        enqueue(&p, "s", "plain").unwrap();
        let items = list(&p, "s").unwrap();
        assert!(items[0].attachments.is_empty());
    }

    /// Drain simulator — mirrors what `ChatQueueWorker::drain_once` does,
    /// minus the Notify wakeups and event emission. Outcomes are scripted
    /// so we can test success / failure / cancel chains without booting a
    /// real Orchestrator.
    fn drain_with(
        db: &DbPool,
        session: &str,
        outcomes: &mut std::collections::HashMap<String, bool>,
    ) -> Vec<String> {
        let mut order = Vec::new();
        while let Some(it) = claim_next(db, session).unwrap() {
            order.push(it.text.clone());
            let ok = outcomes.remove(&it.text).unwrap_or(true);
            if ok {
                mark_done(db, &it.id).unwrap();
            } else {
                mark_failed(db, &it.id).unwrap();
                if !continue_on_error(db) {
                    break;
                }
            }
        }
        order
    }

    #[test]
    fn integration_three_msgs_drain_in_order() {
        let p = test_pool();
        enqueue(&p, "s", "msg1").unwrap();
        enqueue(&p, "s", "msg2").unwrap();
        enqueue(&p, "s", "msg3").unwrap();
        let mut outcomes = std::collections::HashMap::new();
        let order = drain_with(&p, "s", &mut outcomes);
        assert_eq!(order, vec!["msg1", "msg2", "msg3"]);
        assert_eq!(pending_count(&p, "s").unwrap(), 0);
    }

    #[test]
    fn integration_failure_continues_on_default_policy() {
        let p = test_pool();
        // Default policy = continue_on_error = true.
        enqueue(&p, "s", "good1").unwrap();
        enqueue(&p, "s", "boom").unwrap();
        enqueue(&p, "s", "good2").unwrap();
        let mut outcomes = std::collections::HashMap::new();
        outcomes.insert("boom".into(), false);
        let order = drain_with(&p, "s", &mut outcomes);
        assert_eq!(order, vec!["good1", "boom", "good2"]);
        assert_eq!(pending_count(&p, "s").unwrap(), 0);
    }

    #[test]
    fn integration_failure_stops_when_policy_disabled() {
        let p = test_pool();
        crate::db::set_setting(&p, "chat.queue.continue_on_error", "false").unwrap();
        enqueue(&p, "s", "good1").unwrap();
        enqueue(&p, "s", "boom").unwrap();
        enqueue(&p, "s", "good2").unwrap();
        let mut outcomes = std::collections::HashMap::new();
        outcomes.insert("boom".into(), false);
        let order = drain_with(&p, "s", &mut outcomes);
        // Stops after the failed item, leaves "good2" in the queue.
        assert_eq!(order, vec!["good1", "boom"]);
        assert_eq!(pending_count(&p, "s").unwrap(), 1);
        let remaining = list(&p, "s").unwrap();
        assert_eq!(remaining[0].text, "good2");
    }

    #[test]
    fn integration_cancel_mid_queue_skips_target() {
        let p = test_pool();
        let _a = enqueue(&p, "s", "msg1").unwrap();
        let b = enqueue(&p, "s", "msg2").unwrap();
        let _c = enqueue(&p, "s", "msg3").unwrap();
        // Simulate: msg1 starts processing.
        let head = claim_next(&p, "s").unwrap().unwrap();
        assert_eq!(head.text, "msg1");
        // User cancels msg2 while msg1 is still in flight.
        assert!(cancel(&p, &b.id).unwrap());
        // msg1 finishes.
        mark_done(&p, &head.id).unwrap();
        // Drain the rest — must be msg3, msg2 was nuked.
        let mut outcomes = std::collections::HashMap::new();
        let order = drain_with(&p, "s", &mut outcomes);
        assert_eq!(order, vec!["msg3"]);
        assert_eq!(pending_count(&p, "s").unwrap(), 0);
    }
}
