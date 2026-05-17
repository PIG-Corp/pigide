//! Roll-call: broadcast a prompt, collect responses.
//!
//! The flow has two phases so the caller does not block the orchestrator:
//! `start` writes the rollcall row and emits broadcast mail, then later
//! `collect` returns whatever responses landed.

use crate::db::DbPool;
use crate::error::{Error, Result};
use crate::swarm::mailbox;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollcallResponse {
    pub agent_id: String,
    pub body: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollcallSummary {
    pub id: String,
    pub role: String,
    pub prompt: String,
    pub created_at: String,
    pub responses: Vec<RollcallResponse>,
}

pub fn start(db: &DbPool, role: &str, prompt: &str) -> Result<RollcallSummary> {
    if role.trim().is_empty() {
        return Err(Error::Invalid("role required".into()));
    }
    let id = Uuid::new_v4().to_string();
    let ts = Utc::now().to_rfc3339();
    {
        let conn = db.get()?;
        conn.execute(
            "INSERT INTO rollcalls(id, role, prompt, created_at) VALUES(?1,?2,?3,?4)",
            rusqlite::params![&id, role, prompt, &ts],
        )?;
    }
    // Broadcast the prompt with a marker so agents know what to reply to.
    let body = format!("[rollcall:{}] {}", id, prompt);
    mailbox::broadcast(db, None, role, &body)?;
    Ok(RollcallSummary {
        id,
        role: role.to_string(),
        prompt: prompt.to_string(),
        created_at: ts,
        responses: Vec::new(),
    })
}

pub fn respond(db: &DbPool, rollcall_id: &str, agent_id: &str, body: &str) -> Result<()> {
    let conn = db.get()?;
    let ts = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT OR REPLACE INTO rollcall_responses(rollcall_id, agent_id, body, created_at)
         VALUES(?1,?2,?3,?4)",
        rusqlite::params![rollcall_id, agent_id, body, &ts],
    )?;
    Ok(())
}

pub fn collect(db: &DbPool, rollcall_id: &str) -> Result<RollcallSummary> {
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT id, role, prompt, created_at FROM rollcalls WHERE id=?1",
    )?;
    let mut rows = stmt.query([rollcall_id])?;
    let row = rows
        .next()?
        .ok_or_else(|| Error::NotFound(format!("rollcall {}", rollcall_id)))?;
    let id: String = row.get(0)?;
    let role: String = row.get(1)?;
    let prompt: String = row.get(2)?;
    let created_at: String = row.get(3)?;
    drop(rows);
    drop(stmt);
    let mut stmt = conn.prepare(
        "SELECT agent_id, body, created_at FROM rollcall_responses
         WHERE rollcall_id=?1 ORDER BY created_at ASC",
    )?;
    let rsp = stmt.query_map([rollcall_id], |r| {
        Ok(RollcallResponse {
            agent_id: r.get(0)?,
            body: r.get(1)?,
            created_at: r.get(2)?,
        })
    })?;
    let mut responses = Vec::new();
    for row in rsp {
        responses.push(row?);
    }
    Ok(RollcallSummary {
        id,
        role,
        prompt,
        created_at,
        responses,
    })
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
            "CREATE TABLE mailbox (
                id TEXT PRIMARY KEY, from_agent_id TEXT, to_addr TEXT NOT NULL,
                body TEXT NOT NULL, thread_id TEXT, created_at TEXT NOT NULL, read_at TEXT);
             CREATE TABLE rollcalls (
                id TEXT PRIMARY KEY, role TEXT NOT NULL, prompt TEXT NOT NULL,
                created_at TEXT NOT NULL);
             CREATE TABLE rollcall_responses (
                rollcall_id TEXT NOT NULL, agent_id TEXT NOT NULL, body TEXT NOT NULL,
                created_at TEXT NOT NULL, PRIMARY KEY(rollcall_id, agent_id));",
        )
        .unwrap();
        pool
    }

    #[test]
    fn start_then_collect() {
        let p = pool();
        let rc = start(&p, "builder", "ping").unwrap();
        respond(&p, &rc.id, "a1", "ack").unwrap();
        respond(&p, &rc.id, "a2", "ok").unwrap();
        let s = collect(&p, &rc.id).unwrap();
        assert_eq!(s.responses.len(), 2);
        assert_eq!(s.role, "builder");
    }
}
