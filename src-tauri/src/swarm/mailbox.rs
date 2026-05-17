//! Inter-agent mailbox.
//!
//! `to_addr` is either a literal agent UUID or `role:<name>` for broadcasts.
//! Threads (1-on-1 conversations) reuse this table — set `thread_id` on send
//! and filter on read.

use crate::db::DbPool;
use crate::error::{Error, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mail {
    pub id: String,
    pub from_agent_id: Option<String>,
    pub to_addr: String,
    pub body: String,
    pub thread_id: Option<String>,
    pub created_at: String,
    pub read_at: Option<String>,
}

pub fn send(
    db: &DbPool,
    from_agent_id: Option<&str>,
    to_addr: &str,
    body: &str,
    thread_id: Option<&str>,
) -> Result<Mail> {
    if to_addr.trim().is_empty() {
        return Err(Error::Invalid("to_addr required".into()));
    }
    let id = Uuid::new_v4().to_string();
    let ts = Utc::now().to_rfc3339();
    let conn = db.get()?;
    conn.execute(
        "INSERT INTO mailbox(id, from_agent_id, to_addr, body, thread_id, created_at, read_at)
         VALUES(?1,?2,?3,?4,?5,?6,NULL)",
        rusqlite::params![&id, &from_agent_id, to_addr, body, &thread_id, &ts],
    )?;
    Ok(Mail {
        id,
        from_agent_id: from_agent_id.map(String::from),
        to_addr: to_addr.to_string(),
        body: body.to_string(),
        thread_id: thread_id.map(String::from),
        created_at: ts,
        read_at: None,
    })
}

/// Fan-out helper: insert one mail per `to_addr = "role:<role>"`.
pub fn broadcast(
    db: &DbPool,
    from_agent_id: Option<&str>,
    role: &str,
    body: &str,
) -> Result<Mail> {
    let to = format!("role:{}", role);
    send(db, from_agent_id, &to, body, None)
}

pub fn list(
    db: &DbPool,
    to: Option<&str>,
    unread_only: bool,
    limit: i64,
) -> Result<Vec<Mail>> {
    let conn = db.get()?;
    let mut sql = String::from(
        "SELECT id,from_agent_id,to_addr,body,thread_id,created_at,read_at
         FROM mailbox WHERE 1=1",
    );
    let mut params: Vec<String> = Vec::new();
    if let Some(addr) = to {
        sql.push_str(&format!(" AND to_addr = ?{}", params.len() + 1));
        params.push(addr.to_string());
    }
    if unread_only {
        sql.push_str(" AND read_at IS NULL");
    }
    sql.push_str(" ORDER BY created_at DESC LIMIT ");
    sql.push_str(&limit.max(1).min(500).to_string());
    let mut stmt = conn.prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::ToSql> =
        params.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
    let rows = stmt.query_map(refs.as_slice(), |r| {
        Ok(Mail {
            id: r.get(0)?,
            from_agent_id: r.get(1)?,
            to_addr: r.get(2)?,
            body: r.get(3)?,
            thread_id: r.get(4)?,
            created_at: r.get(5)?,
            read_at: r.get(6)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn mark_read(db: &DbPool, ids: &[String]) -> Result<usize> {
    if ids.is_empty() {
        return Ok(0);
    }
    let conn = db.get()?;
    let ts = Utc::now().to_rfc3339();
    let mut n = 0usize;
    let mut stmt = conn.prepare("UPDATE mailbox SET read_at=?2 WHERE id=?1")?;
    for id in ids {
        n += stmt.execute(rusqlite::params![id, &ts])?;
    }
    Ok(n)
}

pub fn list_thread(db: &DbPool, thread_id: &str) -> Result<Vec<Mail>> {
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT id,from_agent_id,to_addr,body,thread_id,created_at,read_at
         FROM mailbox WHERE thread_id=?1 ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map([thread_id], |r| {
        Ok(Mail {
            id: r.get(0)?,
            from_agent_id: r.get(1)?,
            to_addr: r.get(2)?,
            body: r.get(3)?,
            thread_id: r.get(4)?,
            created_at: r.get(5)?,
            read_at: r.get(6)?,
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
            "CREATE TABLE mailbox (
                id TEXT PRIMARY KEY,
                from_agent_id TEXT,
                to_addr TEXT NOT NULL,
                body TEXT NOT NULL,
                thread_id TEXT,
                created_at TEXT NOT NULL,
                read_at TEXT);",
        )
        .unwrap();
        pool
    }

    #[test]
    fn send_and_list_unread() {
        let p = pool();
        send(&p, Some("a1"), "a2", "hi", None).unwrap();
        send(&p, Some("a1"), "a2", "again", None).unwrap();
        let unread = list(&p, Some("a2"), true, 10).unwrap();
        assert_eq!(unread.len(), 2);
    }

    #[test]
    fn mark_read_filters_correctly() {
        let p = pool();
        let m1 = send(&p, None, "a2", "x", None).unwrap();
        let _m2 = send(&p, None, "a2", "y", None).unwrap();
        mark_read(&p, &[m1.id]).unwrap();
        let unread = list(&p, Some("a2"), true, 10).unwrap();
        assert_eq!(unread.len(), 1);
        let all = list(&p, Some("a2"), false, 10).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn broadcast_uses_role_prefix() {
        let p = pool();
        broadcast(&p, None, "builder", "go").unwrap();
        let v = list(&p, Some("role:builder"), false, 10).unwrap();
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn thread_grouping() {
        let p = pool();
        send(&p, Some("a1"), "a2", "hello", Some("t1")).unwrap();
        send(&p, Some("a2"), "a1", "hi", Some("t1")).unwrap();
        send(&p, Some("a1"), "a2", "later", None).unwrap();
        let v = list_thread(&p, "t1").unwrap();
        assert_eq!(v.len(), 2);
    }
}
