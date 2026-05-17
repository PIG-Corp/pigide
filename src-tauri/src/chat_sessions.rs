//! Chat session management — list/create/rename/delete + current-session
//! pointer stored in `settings.current_session_id`.

use crate::db::DbPool;
use crate::error::{Error, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatSession {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub updated_at: String,
    pub message_count: i64,
}

pub fn list(db: &DbPool) -> Result<Vec<ChatSession>> {
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT s.id, s.name, s.created_at, s.updated_at,
                (SELECT COUNT(*) FROM orchestrator_chat c WHERE c.session_id=s.id) AS cnt
         FROM chat_sessions s
         ORDER BY s.updated_at DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(ChatSession {
            id: r.get(0)?,
            name: r.get(1)?,
            created_at: r.get(2)?,
            updated_at: r.get(3)?,
            message_count: r.get(4)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn create(db: &DbPool, name: &str) -> Result<ChatSession> {
    if name.trim().is_empty() {
        return Err(Error::Invalid("session name required".into()));
    }
    let id = Uuid::new_v4().to_string();
    let ts = Utc::now().to_rfc3339();
    let conn = db.get()?;
    conn.execute(
        "INSERT INTO chat_sessions(id,name,created_at,updated_at) VALUES(?1,?2,?3,?3)",
        rusqlite::params![&id, name, &ts],
    )?;
    Ok(ChatSession {
        id,
        name: name.to_string(),
        created_at: ts.clone(),
        updated_at: ts,
        message_count: 0,
    })
}

pub fn rename(db: &DbPool, id: &str, name: &str) -> Result<()> {
    let conn = db.get()?;
    let ts = Utc::now().to_rfc3339();
    let n = conn.execute(
        "UPDATE chat_sessions SET name=?2, updated_at=?3 WHERE id=?1",
        rusqlite::params![id, name, &ts],
    )?;
    if n == 0 {
        return Err(Error::NotFound(format!("session {}", id)));
    }
    Ok(())
}

pub fn delete(db: &DbPool, id: &str) -> Result<()> {
    let conn = db.get()?;
    conn.execute("DELETE FROM chat_sessions WHERE id=?1", [id])?;
    Ok(())
}

pub fn touch(db: &DbPool, id: &str) -> Result<()> {
    let conn = db.get()?;
    let ts = Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE chat_sessions SET updated_at=?2 WHERE id=?1",
        rusqlite::params![id, &ts],
    )?;
    Ok(())
}

/// Return the current session id, creating a default "Main" session if there
/// are none yet. Idempotent and safe to call from multiple places.
pub fn ensure_current(db: &DbPool) -> Result<String> {
    if let Ok(Some(id)) = crate::db::get_setting(db, "current_session_id") {
        if !id.is_empty() {
            // Verify it still exists; otherwise re-elect.
            let conn = db.get()?;
            let exists: i64 = conn.query_row(
                "SELECT COUNT(*) FROM chat_sessions WHERE id=?1",
                [&id],
                |r| r.get(0),
            )?;
            if exists > 0 {
                return Ok(id);
            }
        }
    }
    // No (valid) current; pick the first or create one.
    let list = list(db)?;
    let chosen = if let Some(s) = list.into_iter().next() {
        s.id
    } else {
        create(db, "Main")?.id
    };
    crate::db::set_setting(db, "current_session_id", &chosen)?;
    Ok(chosen)
}

pub fn set_current(db: &DbPool, id: &str) -> Result<()> {
    let conn = db.get()?;
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM chat_sessions WHERE id=?1",
        [id],
        |r| r.get(0),
    )?;
    if exists == 0 {
        return Err(Error::NotFound(format!("session {}", id)));
    }
    drop(conn);
    crate::db::set_setting(db, "current_session_id", id)?;
    Ok(())
}
