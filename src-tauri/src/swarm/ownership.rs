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
use std::path::{Component, Path, PathBuf};

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
    let normalized_path = normalize_workspace_path(db, workspace_id, path)?;
    validate_task_and_agent_scope(db, workspace_id, task_id, agent_id)?;
    let conn = db.get()?;
    let ts = Utc::now().to_rfc3339();
    // Atomic insert: succeeds only when the row does not exist.
    let inserted = conn.execute(
        "INSERT INTO file_ownership(workspace_id, path, task_id, agent_id, acquired_at)
         VALUES(?1,?2,?3,?4,?5)
         ON CONFLICT(workspace_id, path) DO NOTHING",
        rusqlite::params![workspace_id, &normalized_path, task_id, &agent_id, &ts],
    )?;
    if inserted == 1 {
        return Ok(true);
    }
    // Already locked — confirm it is OUR lock.
    let owner: Option<String> = conn
        .query_row(
            "SELECT task_id FROM file_ownership WHERE workspace_id=?1 AND path=?2",
            [workspace_id, &normalized_path],
            |r| r.get(0),
        )
        .ok();
    Ok(owner.as_deref() == Some(task_id))
}

/// Release a file lock held by `task_id`. No-op if `task_id` is not the
/// current holder (so a Reviewer can't accidentally clear a Builder's locks).
pub fn release(db: &DbPool, workspace_id: &str, path: &str, task_id: &str) -> Result<bool> {
    let normalized_path = normalize_workspace_path(db, workspace_id, path)?;
    let conn = db.get()?;
    let n = conn.execute(
        "DELETE FROM file_ownership
         WHERE workspace_id=?1 AND path=?2 AND task_id=?3",
        [workspace_id, &normalized_path, task_id],
    )?;
    Ok(n == 1)
}

/// Drop every lock held by a task. Called when a task is closed (complete,
/// cancelled, or removed).
pub fn release_all_for_task(db: &DbPool, task_id: &str) -> Result<usize> {
    let conn = db.get()?;
    Ok(conn.execute("DELETE FROM file_ownership WHERE task_id=?1", [task_id])?)
}

pub fn who_owns(db: &DbPool, workspace_id: &str, path: &str) -> Result<Option<Ownership>> {
    let normalized_path = normalize_workspace_path(db, workspace_id, path)?;
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT workspace_id, path, task_id, agent_id, acquired_at
         FROM file_ownership WHERE workspace_id=?1 AND path=?2",
    )?;
    let mut rows = stmt.query([workspace_id, &normalized_path])?;
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

fn normalize_workspace_path(db: &DbPool, workspace_id: &str, requested: &str) -> Result<String> {
    if requested.trim().is_empty() {
        return Err(Error::Invalid("path required".into()));
    }
    let roots = workspace_roots(db, workspace_id)?;
    let requested_path = Path::new(requested);
    let absolute = if requested_path.is_absolute() {
        crate::files::validate_workspace_write_path(requested, &roots)?
    } else {
        reject_relative_traversal(requested_path, requested)?;
        let canonical_roots = crate::files::canonicalize_allowed_roots(&roots)?;
        let root = canonical_roots
            .first()
            .ok_or_else(|| Error::Invalid("workspace has no allowed paths".into()))?;
        crate::files::validate_workspace_write_path(
            &root.join(requested_path).to_string_lossy(),
            &roots,
        )?
    };
    let rel = crate::files::to_workspace_relative(&absolute, &roots)?;
    Ok(rel.to_string_lossy().replace('\\', "/"))
}

fn workspace_roots(db: &DbPool, workspace_id: &str) -> Result<Vec<PathBuf>> {
    let ws = crate::workspace::WorkspaceManager::new(db.clone()).get(workspace_id)?;
    let roots = ws
        .paths
        .into_iter()
        .filter(|p| !p.trim().is_empty())
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    if roots.is_empty() {
        return Err(Error::Invalid("workspace has no allowed paths".into()));
    }
    Ok(roots)
}

fn reject_relative_traversal(path: &Path, requested: &str) -> Result<()> {
    if path.components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::CurDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(Error::Invalid(format!(
            "path must be workspace-relative without traversal: {}",
            requested
        )));
    }
    Ok(())
}

fn validate_task_and_agent_scope(
    db: &DbPool,
    workspace_id: &str,
    task_id: &str,
    agent_id: Option<&str>,
) -> Result<()> {
    let conn = db.get()?;
    let task_workspace: String = conn.query_row(
        "SELECT workspace_id FROM tasks WHERE id=?1",
        [task_id],
        |r| r.get(0),
    )?;
    if task_workspace != workspace_id {
        return Err(Error::Invalid(format!(
            "task {} does not belong to workspace {}",
            task_id, workspace_id
        )));
    }
    if let Some(agent_id) = agent_id {
        let agent_workspace: String = conn.query_row(
            "SELECT workspace_id FROM agents WHERE id=?1",
            [agent_id],
            |r| r.get(0),
        )?;
        if agent_workspace != workspace_id {
            return Err(Error::Invalid(format!(
                "agent {} does not belong to workspace {}",
                agent_id, workspace_id
            )));
        }
    }
    Ok(())
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

    fn pool() -> (DbPool, PathBuf) {
        let root = tempdir_for_test("pigide-ownership-root");
        let manager = SqliteConnectionManager::file(root.join("db.sqlite"));
        let pool = r2d2::Pool::builder().max_size(2).build(manager).unwrap();
        let conn = pool.get().unwrap();
        conn.execute_batch(
            "CREATE TABLE workspaces (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                created_at TEXT NOT NULL,
                layout_json TEXT NOT NULL DEFAULT '{\"type\":\"empty\"}',
                paths_json TEXT NOT NULL DEFAULT '[]');
             CREATE TABLE tasks (id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL);
             CREATE TABLE agents (
                id TEXT PRIMARY KEY,
                workspace_id TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'running');
             CREATE TABLE file_ownership (
                workspace_id TEXT NOT NULL,
                path TEXT NOT NULL,
                task_id TEXT NOT NULL,
                agent_id TEXT,
                acquired_at TEXT NOT NULL,
                PRIMARY KEY (workspace_id, path));",
        )
        .unwrap();
        let paths_json = serde_json::to_string(&vec![root.to_string_lossy().to_string()]).unwrap();
        conn.execute(
            "INSERT INTO workspaces(id,name,created_at,layout_json,paths_json)
             VALUES('w1','test','now','{\"type\":\"empty\"}',?1)",
            [&paths_json],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks(id,workspace_id) VALUES('t1','w1'),('t2','w1')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO agents(id,workspace_id,status)
             VALUES('a1','w1','running'),('a2','w1','running')",
            [],
        )
        .unwrap();
        (pool, root)
    }

    #[test]
    fn first_acquirer_wins() {
        let (p, root) = pool();
        assert!(acquire(&p, "w1", "src/a.rs", "t1", None).unwrap());
        assert!(!acquire(&p, "w1", "src/a.rs", "t2", None).unwrap());
        // Same task re-acquires its own lock idempotently.
        assert!(acquire(&p, "w1", "src/a.rs", "t1", None).unwrap());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn release_only_by_owner() {
        let (p, root) = pool();
        acquire(&p, "w1", "src/a.rs", "t1", None).unwrap();
        // Wrong owner — no-op.
        assert!(!release(&p, "w1", "src/a.rs", "t2").unwrap());
        assert!(release(&p, "w1", "src/a.rs", "t1").unwrap());
        // Now t2 can take it.
        assert!(acquire(&p, "w1", "src/a.rs", "t2", None).unwrap());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn release_all_for_task_drops_every_path() {
        let (p, root) = pool();
        acquire(&p, "w1", "a", "t1", None).unwrap();
        acquire(&p, "w1", "b", "t1", None).unwrap();
        acquire(&p, "w1", "c", "t2", None).unwrap();
        let n = release_all_for_task(&p, "t1").unwrap();
        assert_eq!(n, 2);
        assert_eq!(list_for_task(&p, "t1").unwrap().len(), 0);
        assert_eq!(list_for_task(&p, "t2").unwrap().len(), 1);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn who_owns_reports_holder() {
        let (p, root) = pool();
        acquire(&p, "w1", "src/a.rs", "t1", Some("a1")).unwrap();
        let h = who_owns(&p, "w1", "src/a.rs").unwrap().unwrap();
        assert_eq!(h.task_id, "t1");
        assert_eq!(h.agent_id.as_deref(), Some("a1"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn acquire_rejects_traversal_paths() {
        let (p, root) = pool();
        let err = acquire(&p, "w1", "../outside.rs", "t1", None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("traversal"));
        assert!(list_for_workspace(&p, "w1").unwrap().is_empty());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn acquire_rejects_absolute_paths_outside_workspace() {
        let (p, root) = pool();
        let outside = root
            .parent()
            .unwrap()
            .join(format!("pigide-ownership-outside-{}", uuid::Uuid::new_v4()));
        std::fs::write(&outside, "outside").unwrap();

        let err = acquire(&p, "w1", &outside.to_string_lossy(), "t1", None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("outside workspace"));
        assert!(list_for_workspace(&p, "w1").unwrap().is_empty());

        std::fs::remove_dir_all(root).ok();
        std::fs::remove_file(outside).ok();
    }

    fn tempdir_for_test(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("{}-{}", label, uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
