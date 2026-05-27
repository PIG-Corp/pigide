//! `ingest_queue` table accessors. Pure DB; no HTTP, no MemoryService.
//!
//! The fast-lane (`task_complete`, `chat_chunk`) calls `enqueue_task` /
//! `enqueue_chat` after writing a stub. The smart-lane worker calls
//! `pending_for_workspace` to drain a batch, then `mark_processed` /
//! `mark_error` to settle each row.

use crate::db::DbPool;
use crate::error::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    TaskComplete,
    ChatChunk,
}

impl ItemKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ItemKind::TaskComplete => "task_complete",
            ItemKind::ChatChunk => "chat_chunk",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "task_complete" => Some(ItemKind::TaskComplete),
            "chat_chunk" => Some(ItemKind::ChatChunk),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueItem {
    pub id: i64,
    pub workspace_id: String,
    pub kind: String,
    pub payload_json: String,
    pub created_at: String,
    pub smart_attempts: i64,
}

/// Enqueue a `task_complete` item. `note_id` references the fast-lane stub
/// the smart-lane should enrich.
pub fn enqueue_task(db: &DbPool, workspace_id: &str, task_id: &str, note_id: &str) -> Result<i64> {
    let payload = serde_json::json!({
        "task_id": task_id,
        "note_id": note_id,
    });
    insert_row(
        db,
        workspace_id,
        ItemKind::TaskComplete,
        &payload.to_string(),
    )
}

/// Enqueue a `chat_chunk` item.
pub fn enqueue_chat(
    db: &DbPool,
    workspace_id: &str,
    agent_id: &str,
    note_id: &str,
    chunk_no: usize,
) -> Result<i64> {
    let payload = serde_json::json!({
        "agent_id": agent_id,
        "note_id": note_id,
        "chunk_no": chunk_no,
    });
    insert_row(db, workspace_id, ItemKind::ChatChunk, &payload.to_string())
}

fn insert_row(db: &DbPool, workspace_id: &str, kind: ItemKind, payload_json: &str) -> Result<i64> {
    let conn = db.get()?;
    conn.execute(
        "INSERT INTO ingest_queue(workspace_id, kind, payload_json, created_at)
         VALUES(?1, ?2, ?3, ?4)",
        rusqlite::params![
            workspace_id,
            kind.as_str(),
            payload_json,
            Utc::now().to_rfc3339()
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Drain up to `limit` pending rows for `workspace_id`, only those younger
/// than `window_minutes` and with `smart_attempts < 3`. Sorted oldest-first
/// so the worker enriches in roughly chronological order.
pub fn pending_for_workspace(
    db: &DbPool,
    workspace_id: &str,
    window_minutes: i64,
    limit: i64,
) -> Result<Vec<QueueItem>> {
    let cutoff = Utc::now() - chrono::Duration::minutes(window_minutes.max(1));
    let cutoff_iso = cutoff.to_rfc3339();
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT id, workspace_id, kind, payload_json, created_at, smart_attempts
         FROM ingest_queue
         WHERE workspace_id = ?1
           AND processed_at IS NULL
           AND smart_attempts < 3
           AND created_at >= ?2
         ORDER BY id ASC
         LIMIT ?3",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![workspace_id, &cutoff_iso, limit.max(1)],
        |r| {
            Ok(QueueItem {
                id: r.get(0)?,
                workspace_id: r.get(1)?,
                kind: r.get(2)?,
                payload_json: r.get(3)?,
                created_at: r.get(4)?,
                smart_attempts: r.get(5)?,
            })
        },
    )?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Mark a list of row ids `processed_at = now`. Atomic single-statement.
pub fn mark_processed(db: &DbPool, ids: &[i64]) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let placeholders = std::iter::repeat("?")
        .take(ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "UPDATE ingest_queue SET processed_at = ?1 WHERE id IN ({})",
        placeholders
    );
    let now = Utc::now().to_rfc3339();
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![&now];
    for id in ids {
        params.push(id);
    }
    let conn = db.get()?;
    conn.execute(&sql, &*params)?;
    Ok(())
}

/// Mark a list of row ids as failed: bump `smart_attempts`, set `last_error`.
/// Doesn't set `processed_at` so they remain "pending" until they hit 3
/// attempts (then `pending_for_workspace` filters them out).
pub fn mark_error(db: &DbPool, ids: &[i64], err: &str) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let placeholders = std::iter::repeat("?")
        .take(ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "UPDATE ingest_queue
            SET smart_attempts = smart_attempts + 1,
                last_error = ?1
          WHERE id IN ({})",
        placeholders
    );
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![&err];
    for id in ids {
        params.push(id);
    }
    let conn = db.get()?;
    conn.execute(&sql, &*params)?;
    Ok(())
}

/// Total pending rows for a workspace (for status display).
pub fn pending_count(db: &DbPool, workspace_id: &str) -> Result<i64> {
    let conn = db.get()?;
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM ingest_queue
         WHERE workspace_id = ?1 AND processed_at IS NULL AND smart_attempts < 3",
        [workspace_id],
        |r| r.get(0),
    )?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use r2d2_sqlite::SqliteConnectionManager;

    fn fresh_pool() -> DbPool {
        let manager = SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(1).build(manager).unwrap();
        crate::db::migrate_one(&pool.get().unwrap()).unwrap();
        pool
    }

    #[test]
    fn enqueue_task_inserts_row_and_returns_id() {
        let db = fresh_pool();
        let id = enqueue_task(&db, "ws-1", "task-1", "note-1").unwrap();
        assert!(id >= 1);
    }

    #[test]
    fn pending_returns_only_unprocessed_rows_within_window() {
        let db = fresh_pool();
        let i1 = enqueue_task(&db, "ws-1", "t1", "n1").unwrap();
        let _i2 = enqueue_chat(&db, "ws-1", "agent-1", "n2", 1).unwrap();
        let _i3 = enqueue_task(&db, "ws-2", "t3", "n3").unwrap();
        let pending = pending_for_workspace(&db, "ws-1", 30, 50).unwrap();
        assert_eq!(pending.len(), 2);
        // Mark one processed; pending shrinks.
        mark_processed(&db, &[i1]).unwrap();
        let pending = pending_for_workspace(&db, "ws-1", 30, 50).unwrap();
        assert_eq!(pending.len(), 1);
    }

    #[test]
    fn mark_error_increments_attempts_and_filters_after_three() {
        let db = fresh_pool();
        let i1 = enqueue_task(&db, "ws-1", "t1", "n1").unwrap();
        for _ in 0..3 {
            mark_error(&db, &[i1], "boom").unwrap();
        }
        let pending = pending_for_workspace(&db, "ws-1", 30, 50).unwrap();
        assert!(pending.is_empty());
    }

    #[test]
    fn pending_count_excludes_processed_and_exhausted() {
        let db = fresh_pool();
        let _ = enqueue_task(&db, "ws-1", "t1", "n1").unwrap();
        let i2 = enqueue_task(&db, "ws-1", "t2", "n2").unwrap();
        let i3 = enqueue_task(&db, "ws-1", "t3", "n3").unwrap();
        mark_processed(&db, &[i2]).unwrap();
        for _ in 0..3 {
            mark_error(&db, &[i3], "x").unwrap();
        }
        assert_eq!(pending_count(&db, "ws-1").unwrap(), 1);
    }

    #[test]
    fn item_kind_round_trip() {
        for k in [ItemKind::TaskComplete, ItemKind::ChatChunk] {
            assert_eq!(ItemKind::parse(k.as_str()), Some(k));
        }
        assert!(ItemKind::parse("unknown").is_none());
    }
}
