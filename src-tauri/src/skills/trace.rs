//! Per-turn skills telemetry: persists what the router selected, what it
//! rejected, and how many characters of skill content actually made it into
//! the system prompt.

use crate::db::DbPool;
use crate::error::Result;
use crate::skills::router::{RouteResult, Selection};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceRow {
    pub id: String,
    pub session_id: String,
    pub turn_at: String,
    pub selected: Vec<TraceSelection>,
    pub rejected: Vec<TraceSelection>,
    pub composed_chars: i64,
    pub fallback_used: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceSelection {
    pub id: String,
    pub score: f32,
    pub reasons: Vec<String>,
}

impl From<Selection> for TraceSelection {
    fn from(s: Selection) -> Self {
        Self {
            id: s.id,
            score: s.score,
            reasons: s.reasons,
        }
    }
}

/// Idempotently create the `skills_trace` table.
pub fn ensure_table(pool: &DbPool) -> Result<()> {
    let conn = pool.get()?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS skills_trace (
            id              TEXT PRIMARY KEY,
            session_id      TEXT NOT NULL,
            turn_at         TEXT NOT NULL,
            selected_json   TEXT NOT NULL,
            rejected_json   TEXT NOT NULL,
            composed_chars  INTEGER NOT NULL DEFAULT 0,
            fallback_used   INTEGER NOT NULL DEFAULT 0
         );
         CREATE INDEX IF NOT EXISTS idx_skills_trace_session
            ON skills_trace(session_id, turn_at DESC);",
    )?;
    Ok(())
}

/// Record one turn's routing outcome.
pub fn record(
    pool: &DbPool,
    session_id: &str,
    routed: &RouteResult,
    composed_chars: usize,
) -> Result<TraceRow> {
    let id = Uuid::new_v4().to_string();
    let turn_at = Utc::now().to_rfc3339();
    let selected: Vec<TraceSelection> = routed.selected.iter().cloned().map(Into::into).collect();
    let rejected: Vec<TraceSelection> = routed.rejected.iter().cloned().map(Into::into).collect();
    let row = TraceRow {
        id: id.clone(),
        session_id: session_id.to_string(),
        turn_at: turn_at.clone(),
        selected,
        rejected,
        composed_chars: composed_chars as i64,
        fallback_used: routed.fallback_used,
    };
    let conn = pool.get()?;
    conn.execute(
        "INSERT INTO skills_trace
            (id, session_id, turn_at, selected_json, rejected_json,
             composed_chars, fallback_used)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            row.id,
            row.session_id,
            row.turn_at,
            serde_json::to_string(&row.selected)?,
            serde_json::to_string(&row.rejected)?,
            row.composed_chars,
            row.fallback_used as i32,
        ],
    )?;
    Ok(row)
}

/// Fetch the latest trace, optionally filtered by session.
pub fn latest(pool: &DbPool, session_id: Option<&str>) -> Result<Option<TraceRow>> {
    let conn = pool.get()?;
    let (sql, params): (&str, Vec<Value>) = match session_id {
        Some(s) => (
            "SELECT id, session_id, turn_at, selected_json, rejected_json,
                    composed_chars, fallback_used
             FROM skills_trace WHERE session_id=?1
             ORDER BY turn_at DESC LIMIT 1",
            vec![json!(s)],
        ),
        None => (
            "SELECT id, session_id, turn_at, selected_json, rejected_json,
                    composed_chars, fallback_used
             FROM skills_trace
             ORDER BY turn_at DESC LIMIT 1",
            vec![],
        ),
    };
    let mut stmt = conn.prepare(sql)?;
    let mut row_iter = stmt.query_map(
        rusqlite::params_from_iter(params.iter().map(|v| match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })),
        |r| {
            Ok(TraceRow {
                id: r.get(0)?,
                session_id: r.get(1)?,
                turn_at: r.get(2)?,
                selected: serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or_default(),
                rejected: serde_json::from_str(&r.get::<_, String>(4)?).unwrap_or_default(),
                composed_chars: r.get(5)?,
                fallback_used: r.get::<_, i32>(6)? != 0,
            })
        },
    )?;
    if let Some(row) = row_iter.next() {
        return Ok(Some(row?));
    }
    Ok(None)
}
