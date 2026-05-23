//! Review gates: BridgeSwarm's "Reviewer must PASS before merge" rule.
//!
//! Each gate is keyed to a task and resolves to one of {pending, pass, fail}.
//! The Coordinator/Builder calls `task_completable` before flipping a task to
//! `complete` — if any gate is not `pass`, the task is held in `in_review`.
//!
//! Multiple gates can be opened on the same task (e.g. one per Reviewer),
//! and the task is completable only when ALL gates are `pass`. An empty set
//! is treated as "not gated" — only tasks that explicitly opted into review
//! are blocked.

use crate::db::DbPool;
use crate::error::{Error, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Pending,
    Pass,
    Fail,
}

impl Verdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            Verdict::Pending => "pending",
            Verdict::Pass => "pass",
            Verdict::Fail => "fail",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Verdict::Pending),
            "pass" => Some(Verdict::Pass),
            "fail" => Some(Verdict::Fail),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewGate {
    pub id: String,
    pub task_id: String,
    pub reviewer_id: Option<String>,
    pub verdict: Verdict,
    pub reason: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Open a fresh review gate on a task. Multiple gates can be opened on the
/// same task — the Coordinator chooses how many reviewers must pass.
pub fn open(db: &DbPool, task_id: &str, reviewer_id: Option<&str>) -> Result<ReviewGate> {
    if task_id.trim().is_empty() {
        return Err(Error::Invalid("task_id required".into()));
    }
    let id = Uuid::new_v4().to_string();
    let ts = Utc::now().to_rfc3339();
    let conn = db.get()?;
    conn.execute(
        "INSERT INTO review_gates(id, task_id, reviewer_id, verdict, reason, created_at, updated_at)
         VALUES(?1,?2,?3,'pending','',?4,?4)",
        rusqlite::params![&id, task_id, &reviewer_id, &ts],
    )?;
    Ok(ReviewGate {
        id,
        task_id: task_id.to_string(),
        reviewer_id: reviewer_id.map(String::from),
        verdict: Verdict::Pending,
        reason: String::new(),
        created_at: ts.clone(),
        updated_at: ts,
    })
}

pub fn vote(db: &DbPool, gate_id: &str, verdict: Verdict, reason: &str) -> Result<ReviewGate> {
    let ts = Utc::now().to_rfc3339();
    let conn = db.get()?;
    let n = conn.execute(
        "UPDATE review_gates
         SET verdict=?2, reason=?3, updated_at=?4
         WHERE id=?1",
        rusqlite::params![gate_id, verdict.as_str(), reason, &ts],
    )?;
    if n == 0 {
        return Err(Error::NotFound(format!("review_gate {}", gate_id)));
    }
    get(db, gate_id)
}

pub fn get(db: &DbPool, gate_id: &str) -> Result<ReviewGate> {
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT id, task_id, reviewer_id, verdict, reason, created_at, updated_at
         FROM review_gates WHERE id=?1",
    )?;
    let mut rows = stmt.query([gate_id])?;
    let r = rows
        .next()?
        .ok_or_else(|| Error::NotFound(format!("review_gate {}", gate_id)))?;
    let verdict_str: String = r.get(3)?;
    Ok(ReviewGate {
        id: r.get(0)?,
        task_id: r.get(1)?,
        reviewer_id: r.get(2)?,
        verdict: Verdict::parse(&verdict_str).unwrap_or(Verdict::Pending),
        reason: r.get(4)?,
        created_at: r.get(5)?,
        updated_at: r.get(6)?,
    })
}

pub fn list_for_task(db: &DbPool, task_id: &str) -> Result<Vec<ReviewGate>> {
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT id, task_id, reviewer_id, verdict, reason, created_at, updated_at
         FROM review_gates WHERE task_id=?1 ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map([task_id], |r| {
        let verdict_str: String = r.get(3)?;
        Ok(ReviewGate {
            id: r.get(0)?,
            task_id: r.get(1)?,
            reviewer_id: r.get(2)?,
            verdict: Verdict::parse(&verdict_str).unwrap_or(Verdict::Pending),
            reason: r.get(4)?,
            created_at: r.get(5)?,
            updated_at: r.get(6)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Decide whether a task can transition to `complete`. Returns Ok(()) when
/// every gate is `pass`. Returns Err(Invalid) listing the blockers when one
/// or more gates is `pending` or `fail`. A task with zero gates is treated
/// as ungated and always passes.
pub fn task_completable(db: &DbPool, task_id: &str) -> Result<()> {
    let gates = list_for_task(db, task_id)?;
    if gates.is_empty() {
        return Ok(());
    }
    let mut blockers = Vec::new();
    for g in &gates {
        match g.verdict {
            Verdict::Pass => {}
            Verdict::Pending => {
                blockers.push(format!("gate {} pending", g.id));
            }
            Verdict::Fail => {
                blockers.push(format!(
                    "gate {} FAIL: {}",
                    g.id,
                    if g.reason.is_empty() {
                        "(no reason)"
                    } else {
                        &g.reason
                    }
                ));
            }
        }
    }
    if blockers.is_empty() {
        Ok(())
    } else {
        Err(Error::Invalid(format!(
            "review gate(s) blocking task {}: {}",
            task_id,
            blockers.join("; ")
        )))
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
            "CREATE TABLE tasks (id TEXT PRIMARY KEY);
             CREATE TABLE agents (id TEXT PRIMARY KEY);
             CREATE TABLE review_gates (
                id TEXT PRIMARY KEY,
                task_id TEXT NOT NULL,
                reviewer_id TEXT,
                verdict TEXT NOT NULL DEFAULT 'pending',
                reason TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL);
             INSERT INTO tasks(id) VALUES('t1');",
        )
        .unwrap();
        pool
    }

    #[test]
    fn ungated_task_is_completable() {
        let p = pool();
        assert!(task_completable(&p, "t1").is_ok());
    }

    #[test]
    fn pending_gate_blocks_completion() {
        let p = pool();
        open(&p, "t1", None).unwrap();
        let err = task_completable(&p, "t1").unwrap_err().to_string();
        assert!(err.contains("pending"));
    }

    #[test]
    fn fail_blocks_with_reason() {
        let p = pool();
        let g = open(&p, "t1", None).unwrap();
        vote(&p, &g.id, Verdict::Fail, "tests are red").unwrap();
        let err = task_completable(&p, "t1").unwrap_err().to_string();
        assert!(err.contains("FAIL"));
        assert!(err.contains("tests are red"));
    }

    #[test]
    fn all_pass_unblocks() {
        let p = pool();
        let g1 = open(&p, "t1", Some("r1")).unwrap();
        let g2 = open(&p, "t1", Some("r2")).unwrap();
        vote(&p, &g1.id, Verdict::Pass, "lgtm").unwrap();
        vote(&p, &g2.id, Verdict::Pass, "ship it").unwrap();
        assert!(task_completable(&p, "t1").is_ok());
    }
}
