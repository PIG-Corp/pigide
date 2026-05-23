//! Reusable prompt library (BridgeSpace gap #18).
//!
//! Workspace-scoped (NULL = global) named snippets that can be inserted into
//! the orchestrator chat or fed to a spawning agent as initial context.
//!
//! Names are unique within their scope: `(workspace_id, name)` for a
//! workspace prompt, or `('', name)` for a global one. The
//! `idx_prompts_ws_name` migration uses `COALESCE(workspace_id, '')` to make
//! that uniqueness work across the NULL / non-NULL split.

use crate::db::DbPool;
use crate::error::{Error, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prompt {
    pub id: String,
    pub workspace_id: Option<String>,
    pub name: String,
    pub body: String,
    pub tags: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Prompt> {
    let tags_json: String = row.get(4)?;
    let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
    Ok(Prompt {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        name: row.get(2)?,
        body: row.get(3)?,
        tags,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

pub fn create(
    db: &DbPool,
    workspace_id: Option<&str>,
    name: &str,
    body: &str,
    tags: &[String],
) -> Result<Prompt> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::Invalid("prompt name required".into()));
    }
    let id = Uuid::new_v4().to_string();
    let ts = Utc::now().to_rfc3339();
    let tags_json = serde_json::to_string(tags)?;
    let conn = db.get()?;
    conn.execute(
        "INSERT INTO prompts(id, workspace_id, name, body, tags_json, created_at, updated_at)
         VALUES(?1,?2,?3,?4,?5,?6,?6)",
        rusqlite::params![&id, &workspace_id, name, body, &tags_json, &ts],
    )
    .map_err(|e| {
        // Surface a friendlier error on UNIQUE conflicts.
        if let rusqlite::Error::SqliteFailure(_, Some(msg)) = &e {
            if msg.contains("idx_prompts_ws_name") {
                return Error::Invalid(format!("prompt named '{}' already exists", name));
            }
        }
        Error::from(e)
    })?;
    Ok(Prompt {
        id,
        workspace_id: workspace_id.map(String::from),
        name: name.to_string(),
        body: body.to_string(),
        tags: tags.to_vec(),
        created_at: ts.clone(),
        updated_at: ts,
    })
}

pub fn update(
    db: &DbPool,
    id: &str,
    name: Option<&str>,
    body: Option<&str>,
    tags: Option<&[String]>,
) -> Result<Prompt> {
    let conn = db.get()?;
    let ts = Utc::now().to_rfc3339();
    if let Some(n) = name {
        let n = n.trim();
        if n.is_empty() {
            return Err(Error::Invalid("prompt name required".into()));
        }
        conn.execute(
            "UPDATE prompts SET name=?1, updated_at=?2 WHERE id=?3",
            rusqlite::params![n, &ts, id],
        )?;
    }
    if let Some(b) = body {
        conn.execute(
            "UPDATE prompts SET body=?1, updated_at=?2 WHERE id=?3",
            rusqlite::params![b, &ts, id],
        )?;
    }
    if let Some(t) = tags {
        let tags_json = serde_json::to_string(t)?;
        conn.execute(
            "UPDATE prompts SET tags_json=?1, updated_at=?2 WHERE id=?3",
            rusqlite::params![&tags_json, &ts, id],
        )?;
    }
    get(db, id)
}

pub fn delete(db: &DbPool, id: &str) -> Result<bool> {
    let conn = db.get()?;
    let n = conn.execute("DELETE FROM prompts WHERE id=?1", [id])?;
    Ok(n == 1)
}

pub fn get(db: &DbPool, id: &str) -> Result<Prompt> {
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT id, workspace_id, name, body, tags_json, created_at, updated_at
         FROM prompts WHERE id=?1",
    )?;
    let mut rows = stmt.query([id])?;
    let row = rows
        .next()?
        .ok_or_else(|| Error::NotFound(format!("prompt {}", id)))?;
    Ok(from_row(row)?)
}

/// List prompts visible to `workspace_id`. When `workspace_id` is provided,
/// returns workspace-scoped prompts and any global ones (`workspace_id IS
/// NULL`) so a user can keep cross-project favourites.
pub fn list(db: &DbPool, workspace_id: Option<&str>, tag: Option<&str>) -> Result<Vec<Prompt>> {
    let conn = db.get()?;
    let prompts = match workspace_id {
        Some(ws) => {
            let mut stmt = conn.prepare(
                "SELECT id, workspace_id, name, body, tags_json, created_at, updated_at
                 FROM prompts WHERE workspace_id IS NULL OR workspace_id=?1
                 ORDER BY name ASC",
            )?;
            let rows = stmt.query_map([ws], from_row)?;
            let mut v = Vec::new();
            for r in rows {
                v.push(r?);
            }
            v
        }
        None => {
            let mut stmt = conn.prepare(
                "SELECT id, workspace_id, name, body, tags_json, created_at, updated_at
                 FROM prompts WHERE workspace_id IS NULL ORDER BY name ASC",
            )?;
            let rows = stmt.query_map([], from_row)?;
            let mut v = Vec::new();
            for r in rows {
                v.push(r?);
            }
            v
        }
    };
    let out = match tag {
        Some(t) => prompts
            .into_iter()
            .filter(|p| p.tags.iter().any(|x| x == t))
            .collect(),
        None => prompts,
    };
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
            "CREATE TABLE workspaces (id TEXT PRIMARY KEY);
             CREATE TABLE prompts (
                id            TEXT PRIMARY KEY,
                workspace_id  TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
                name          TEXT NOT NULL,
                body          TEXT NOT NULL,
                tags_json     TEXT NOT NULL DEFAULT '[]',
                created_at    TEXT NOT NULL,
                updated_at    TEXT NOT NULL);
             CREATE UNIQUE INDEX idx_prompts_ws_name
                ON prompts(COALESCE(workspace_id,''), name);
             INSERT INTO workspaces(id) VALUES('w1');",
        )
        .unwrap();
        pool
    }

    #[test]
    fn create_and_get() {
        let p = pool();
        let made = create(&p, Some("w1"), "Refactor", "body", &["meta".into()]).unwrap();
        let again = get(&p, &made.id).unwrap();
        assert_eq!(again.name, "Refactor");
        assert_eq!(again.tags, vec!["meta".to_string()]);
    }

    #[test]
    fn duplicate_name_in_scope_rejected() {
        let p = pool();
        create(&p, Some("w1"), "X", "a", &[]).unwrap();
        let err = create(&p, Some("w1"), "X", "b", &[]).unwrap_err();
        assert!(matches!(err, Error::Invalid(_)));
    }

    #[test]
    fn global_and_workspace_can_share_name() {
        let p = pool();
        create(&p, None, "Shared", "global", &[]).unwrap();
        // Same name in a workspace scope is fine.
        create(&p, Some("w1"), "Shared", "ws", &[]).unwrap();
    }

    #[test]
    fn list_returns_globals_plus_workspace() {
        let p = pool();
        create(&p, None, "G", "g", &[]).unwrap();
        create(&p, Some("w1"), "W", "w", &[]).unwrap();
        let visible = list(&p, Some("w1"), None).unwrap();
        let names: Vec<_> = visible.iter().map(|p| p.name.clone()).collect();
        assert!(names.contains(&"G".to_string()));
        assert!(names.contains(&"W".to_string()));
    }

    #[test]
    fn update_changes_fields() {
        let p = pool();
        let made = create(&p, None, "X", "before", &[]).unwrap();
        let updated = update(&p, &made.id, None, Some("after"), Some(&["t".into()])).unwrap();
        assert_eq!(updated.body, "after");
        assert_eq!(updated.tags, vec!["t".to_string()]);
    }

    #[test]
    fn delete_removes_row() {
        let p = pool();
        let made = create(&p, None, "X", "y", &[]).unwrap();
        assert!(delete(&p, &made.id).unwrap());
        assert!(matches!(get(&p, &made.id).unwrap_err(), Error::NotFound(_)));
    }

    #[test]
    fn list_filters_by_tag() {
        let p = pool();
        create(&p, None, "A", "a", &["alpha".into()]).unwrap();
        create(&p, None, "B", "b", &["beta".into()]).unwrap();
        let alpha = list(&p, None, Some("alpha")).unwrap();
        assert_eq!(alpha.len(), 1);
        assert_eq!(alpha[0].name, "A");
    }
}
