//! File ownership: BridgeSwarm's "exclusive per-task" file lock.
//!
//! BridgeSwarm avoids merge conflicts by giving each task an exclusive
//! checkout on the files it touches; overlapping tasks are sequenced rather
//! than running in parallel. PigSwarm's port stores those locks in the
//! `file_ownership` table — workspace-relative path is the key, the holder is
//! a (task_id, optional agent_id) pair.
//!
//! `acquire` is atomic via `INSERT … ON CONFLICT DO NOTHING`; the caller can
//! tell from the `bool` return value whether it owns the file or somebody
//! else does. `who_owns` answers "who do I escalate to?" — Coordinator uses
//! it to decide whether to wait or re-assign.

use crate::db::DbPool;
use crate::error::{Error, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ownership {
    pub workspace_id: String,
    pub path: String,
    pub task_id: String,
    pub agent_id: Option<String>,
    pub acquired_at: String,
}

/// Try to take an exclusive lock on `path` for `task_id`. Returns true when
/// the lock is now held by `task_id` (either freshly acquired or already
/// held). Returns false when another task holds it.
pub fn acquire(
    db: &DbPool,
    workspace_id: &str,
    path: &str,
    task_id: &str,
    agent_id: Option<&str>,
) -> Result<bool> {
    if path.trim().is_empty() {
        return Err(Error::Invalid("path required".into()));
    }
    let conn = db.get()?;
    let ts = Utc::now().to_rfc3339();
    // Atomic insert: succeeds only when the row does not exist.
    let inserted = conn.execute(
        "INSERT INTO file_ownership(workspace_id, path, task_id, agent_id, acquired_at)
         VALUES(?1,?2,?3,?4,?5)
         ON CONFLICT(workspace_id, path) DO NOTHING",
        rusqlite::params![workspace_id, path, task_id, &agent_id, &ts],
    )?;
    if inserted == 1 {
        return Ok(true);
    }
    // Already locked — confirm it is OUR lock.
    let owner: Option<String> = conn
        .query_row(
            "SELECT task_id FROM file_ownership WHERE workspace_id=?1 AND path=?2",
            [workspace_id, path],
            |r| r.get(0),
        )
        .ok();
    Ok(owner.as_deref() == Some(task_id))
}

/// Release a file lock held by `task_id`. No-op if `task_id` is not the
/// current holder (so a Reviewer can't accidentally clear a Builder's locks).
pub fn release(
    db: &DbPool,
    workspace_id: &str,
    path: &str,
    task_id: &str,
) -> Result<bool> {
    let conn = db.get()?;
    let n = conn.execute(
        "DELETE FROM file_ownership
         WHERE workspace_id=?1 AND path=?2 AND task_id=?3",
        [workspace_id, path, task_id],
    )?;
    Ok(n == 1)
}

/// Drop every lock held by a task. Called when a task is closed (complete,
/// cancelled, or removed).
pub fn release_all_for_task(db: &DbPool, task_id: &str) -> Result<usize> {
    let conn = db.get()?;
    Ok(conn.execute(
        "DELETE FROM file_ownership WHERE task_id=?1",
        [task_id],
    )?)
}

pub fn who_owns(
    db: &DbPool,
    workspace_id: &str,
    path: &str,
) -> Result<Option<Ownership>> {
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT workspace_id, path, task_id, agent_id, acquired_at
         FROM file_ownership WHERE workspace_id=?1 AND path=?2",
    )?;
    let mut rows = stmt.query([workspace_id, path])?;
    if let Some(r) = rows.next()? {
        Ok(Some(Ownership {
            workspace_id: r.get(0)?,
            path: r.get(1)?,
            task_id: r.get(2)?,
            agent_id: r.get(3)?,
            acquired_at: r.get(4)?,
        }))
    } else {
        Ok(None)
    }
}

pub fn list_for_task(db: &DbPool, task_id: &str) -> Result<Vec<Ownership>> {
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT workspace_id, path, task_id, agent_id, acquired_at
         FROM file_ownership WHERE task_id=?1 ORDER BY acquired_at ASC",
    )?;
    let rows = stmt.query_map([task_id], |r| {
        Ok(Ownership {
            workspace_id: r.get(0)?,
            path: r.get(1)?,
            task_id: r.get(2)?,
            agent_id: r.get(3)?,
            acquired_at: r.get(4)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn list_for_workspace(db: &DbPool, workspace_id: &str) -> Result<Vec<Ownership>> {
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT workspace_id, path, task_id, agent_id, acquired_at
         FROM file_ownership WHERE workspace_id=?1 ORDER BY acquired_at ASC",
    )?;
    let rows = stmt.query_map([workspace_id], |r| {
        Ok(Ownership {
            workspace_id: r.get(0)?,
            path: r.get(1)?,
            task_id: r.get(2)?,
            agent_id: r.get(3)?,
            acquired_at: r.get(4)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use r2d2_sqlite::SqliteConnectionManager;

    fn pool() -> DbPool {
        let manager = SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(2).build(manager).unwrap();
        let conn = pool.get().unwrap();
        conn.execute_batch(
            "CREATE TABLE tasks (id TEXT PRIMARY KEY);
             CREATE TABLE agents (id TEXT PRIMARY KEY);
             CREATE TABLE file_ownership (
                workspace_id TEXT NOT NULL,
                path TEXT NOT NULL,
                task_id TEXT NOT NULL,
                agent_id TEXT,
                acquired_at TEXT NOT NULL,
                PRIMARY KEY (workspace_id, path));
             INSERT INTO tasks(id) VALUES('t1'),('t2');",
        )
        .unwrap();
        pool
    }

    #[test]
    fn first_acquirer_wins() {
        let p = pool();
        assert!(acquire(&p, "w1", "src/a.rs", "t1", None).unwrap());
        assert!(!acquire(&p, "w1", "src/a.rs", "t2", None).unwrap());
        // Same task re-acquires its own lock idempotently.
        assert!(acquire(&p, "w1", "src/a.rs", "t1", None).unwrap());
    }

    #[test]
    fn release_only_by_owner() {
        let p = pool();
        acquire(&p, "w1", "src/a.rs", "t1", None).unwrap();
        // Wrong owner — no-op.
        assert!(!release(&p, "w1", "src/a.rs", "t2").unwrap());
        assert!(release(&p, "w1", "src/a.rs", "t1").unwrap());
        // Now t2 can take it.
        assert!(acquire(&p, "w1", "src/a.rs", "t2", None).unwrap());
    }

    #[test]
    fn release_all_for_task_drops_every_path() {
        let p = pool();
        acquire(&p, "w1", "a", "t1", None).unwrap();
        acquire(&p, "w1", "b", "t1", None).unwrap();
        acquire(&p, "w1", "c", "t2", None).unwrap();
        let n = release_all_for_task(&p, "t1").unwrap();
        assert_eq!(n, 2);
        assert_eq!(list_for_task(&p, "t1").unwrap().len(), 0);
        assert_eq!(list_for_task(&p, "t2").unwrap().len(), 1);
    }

    #[test]
    fn who_owns_reports_holder() {
        let p = pool();
        acquire(&p, "w1", "src/a.rs", "t1", Some("a1")).unwrap();
        let h = who_owns(&p, "w1", "src/a.rs").unwrap().unwrap();
        assert_eq!(h.task_id, "t1");
        assert_eq!(h.agent_id.as_deref(), Some("a1"));
    }
}
