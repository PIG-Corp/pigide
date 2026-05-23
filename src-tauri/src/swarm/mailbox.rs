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
    from_agent_id: &str,
    to_addr: &str,
    body: &str,
    thread_id: Option<&str>,
) -> Result<Mail> {
    validate_agent(db, from_agent_id)?;
    insert(db, Some(from_agent_id), to_addr, body, thread_id)
}

pub fn send_system(
    db: &DbPool,
    to_addr: &str,
    body: &str,
    thread_id: Option<&str>,
) -> Result<Mail> {
    insert(db, None, to_addr, body, thread_id)
}

fn insert(
    db: &DbPool,
    from_agent_id: Option<&str>,
    to_addr: &str,
    body: &str,
    thread_id: Option<&str>,
) -> Result<Mail> {
    if to_addr.trim().is_empty() {
        return Err(Error::Invalid("to_addr required".into()));
    }
    validate_to_addr(db, to_addr)?;
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
pub fn broadcast(db: &DbPool, from_agent_id: &str, role: &str, body: &str) -> Result<Mail> {
    validate_agent(db, from_agent_id)?;
    validate_role(role)?;
    let to = format!("role:{}", role);
    insert(db, Some(from_agent_id), &to, body, None)
}

pub fn broadcast_system(db: &DbPool, role: &str, body: &str) -> Result<Mail> {
    validate_role(role)?;
    let to = format!("role:{}", role);
    insert(db, None, &to, body, None)
}

pub fn list(db: &DbPool, to: Option<&str>, unread_only: bool, limit: i64) -> Result<Vec<Mail>> {
    let conn = db.get()?;
    let mut role: Option<String> = None;
    if let Some(addr) = to {
        if !addr.starts_with("role:") {
            let mut stmt = conn.prepare("SELECT role FROM agents WHERE id = ?1")?;
            let mut rows = stmt.query([addr])?;
            if let Some(row) = rows.next()? {
                role = Some(row.get(0)?);
            }
        }
    }

    let mut sql = String::from(
        "SELECT id,from_agent_id,to_addr,body,thread_id,created_at,read_at
         FROM mailbox WHERE 1=1",
    );
    let mut params: Vec<String> = Vec::new();
    if let Some(addr) = to {
        if let Some(ref r) = role {
            let role_addr = format!("role:{}", r);
            sql.push_str(&format!(
                " AND (to_addr = ?{} OR to_addr = ?{})",
                params.len() + 1,
                params.len() + 2
            ));
            params.push(addr.to_string());
            params.push(role_addr);
        } else {
            sql.push_str(&format!(" AND to_addr = ?{}", params.len() + 1));
            params.push(addr.to_string());
        }
    }
    if unread_only {
        sql.push_str(" AND read_at IS NULL");
    }
    sql.push_str(" ORDER BY created_at DESC LIMIT ");
    sql.push_str(&limit.clamp(1, 500).to_string());
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

pub fn list_for_reader(
    db: &DbPool,
    reader_agent_id: &str,
    to: &str,
    unread_only: bool,
    limit: i64,
) -> Result<Vec<Mail>> {
    validate_mailbox_access(db, reader_agent_id, to)?;
    list(db, Some(to), unread_only, limit)
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

pub fn mark_read_for_reader(db: &DbPool, reader_agent_id: &str, ids: &[String]) -> Result<usize> {
    if ids.is_empty() {
        return Ok(0);
    }
    let role = agent_role(db, reader_agent_id)?;
    let role_addr = format!("role:{}", role);
    let conn = db.get()?;
    let ts = Utc::now().to_rfc3339();
    let mut n = 0usize;
    let mut stmt = conn.prepare(
        "UPDATE mailbox SET read_at=?2
         WHERE id=?1 AND (to_addr=?3 OR to_addr=?4)",
    )?;
    for id in ids {
        n += stmt.execute(rusqlite::params![id, &ts, reader_agent_id, &role_addr])?;
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

fn validate_mailbox_access(db: &DbPool, reader_agent_id: &str, to_addr: &str) -> Result<()> {
    let role = agent_role(db, reader_agent_id)?;
    if to_addr == reader_agent_id {
        return Ok(());
    }
    if let Some(target_role) = to_addr.strip_prefix("role:") {
        validate_role(target_role)?;
        if target_role == role {
            return Ok(());
        }
    }
    Err(Error::Invalid(format!(
        "agent {} cannot read mailbox {}",
        reader_agent_id, to_addr
    )))
}

fn validate_to_addr(db: &DbPool, to_addr: &str) -> Result<()> {
    if let Some(role) = to_addr.strip_prefix("role:") {
        return validate_role(role);
    }
    validate_agent(db, to_addr)
}

fn validate_agent(db: &DbPool, agent_id: &str) -> Result<()> {
    let _ = agent_role(db, agent_id)?;
    Ok(())
}

fn agent_role(db: &DbPool, agent_id: &str) -> Result<String> {
    if agent_id.trim().is_empty() {
        return Err(Error::Invalid("agent_id required".into()));
    }
    let conn = db.get()?;
    conn.query_row("SELECT role FROM agents WHERE id=?1", [agent_id], |r| {
        r.get(0)
    })
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Error::NotFound(format!("agent {}", agent_id)),
        other => Error::Db(other),
    })
}

fn validate_role(role: &str) -> Result<()> {
    match role {
        "coordinator" | "builder" | "reviewer" | "scout" => Ok(()),
        _ => Err(Error::Invalid(format!("invalid role: {}", role))),
    }
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
                read_at TEXT);
             CREATE TABLE agents (
                id TEXT PRIMARY KEY,
                role TEXT NOT NULL); 
             INSERT INTO agents(id, role)
             VALUES('a1','builder'),('a2','reviewer'),('c1','coordinator');",
        )
        .unwrap();
        pool
    }

    #[test]
    fn send_and_list_unread() {
        let p = pool();
        send(&p, "a1", "a2", "hi", None).unwrap();
        send(&p, "a1", "a2", "again", None).unwrap();
        let unread = list(&p, Some("a2"), true, 10).unwrap();
        assert_eq!(unread.len(), 2);
        assert_eq!(unread[0].from_agent_id.as_deref(), Some("a1"));
    }

    #[test]
    fn mark_read_filters_correctly() {
        let p = pool();
        let m1 = send(&p, "a1", "a2", "x", None).unwrap();
        let _m2 = send(&p, "a1", "a2", "y", None).unwrap();
        mark_read(&p, &[m1.id]).unwrap();
        let unread = list(&p, Some("a2"), true, 10).unwrap();
        assert_eq!(unread.len(), 1);
        let all = list(&p, Some("a2"), false, 10).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn broadcast_uses_role_prefix() {
        let p = pool();
        broadcast(&p, "a1", "builder", "go").unwrap();
        let v = list(&p, Some("role:builder"), false, 10).unwrap();
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn thread_grouping() {
        let p = pool();
        send(&p, "a1", "a2", "hello", Some("t1")).unwrap();
        send(&p, "a2", "a1", "hi", Some("t1")).unwrap();
        send(&p, "a1", "a2", "later", None).unwrap();
        let v = list_thread(&p, "t1").unwrap();
        assert_eq!(v.len(), 2);
    }

    #[test]
    fn send_requires_valid_sender() {
        let p = pool();
        let err = send(&p, "missing", "a2", "hi", None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("agent missing"));
    }

    #[test]
    fn reader_can_only_read_own_mailbox_or_role_mailbox() {
        let p = pool();
        send(&p, "a1", "a2", "private", None).unwrap();
        broadcast(&p, "a1", "reviewer", "role mail").unwrap();

        assert_eq!(list_for_reader(&p, "a2", "a2", true, 10).unwrap().len(), 2);
        assert_eq!(
            list_for_reader(&p, "a2", "role:reviewer", true, 10)
                .unwrap()
                .len(),
            1
        );
        let err = list_for_reader(&p, "a1", "a2", true, 10)
            .unwrap_err()
            .to_string();
        assert!(err.contains("cannot read mailbox"));
    }

    #[test]
    fn agent_receives_role_broadcasts() {
        let p = pool();
        // a1 is 'builder', a2 is 'reviewer'
        send(&p, "a2", "a1", "direct to builder a1", None).unwrap();
        broadcast(&p, "a2", "builder", "broadcast to builders").unwrap();
        broadcast(&p, "a2", "reviewer", "broadcast to reviewers").unwrap();

        // When listing for a1, it should return both direct to a1 and broadcast to builder
        let mails = list(&p, Some("a1"), false, 10).unwrap();
        assert_eq!(mails.len(), 2);
        let bodies: Vec<String> = mails.iter().map(|m| m.body.clone()).collect();
        assert!(bodies.contains(&"direct to builder a1".to_string()));
        assert!(bodies.contains(&"broadcast to builders".to_string()));
        assert!(!bodies.contains(&"broadcast to reviewers".to_string()));
    }
}
