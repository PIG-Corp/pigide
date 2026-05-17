//! Transcription history: list/search/export with FTS5.

use crate::db::DbPool;
use crate::error::{Error, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transcript {
    pub id: String,
    pub text: String,
    pub text_raw: String,
    pub language: Option<String>,
    pub model_id: String,
    pub source: String,
    pub duration_ms: i64,
    pub word_count: i64,
    pub created_at: String,
    pub injected: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct VoiceStats {
    pub sessions: i64,
    pub total_words: i64,
    pub talk_seconds: f64,
    pub avg_wpm: f64,
}

pub fn insert(
    db: &DbPool,
    text: &str,
    text_raw: &str,
    language: Option<&str>,
    model_id: &str,
    source: &str,
    duration_ms: i64,
) -> Result<Transcript> {
    let id = Uuid::new_v4().to_string();
    let ts = Utc::now().to_rfc3339();
    let word_count = text.split_whitespace().count() as i64;
    let conn = db.get()?;
    conn.execute(
        "INSERT INTO voice_transcripts(id,text,text_raw,language,model_id,source,
                                       duration_ms,word_count,created_at,injected)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,0)",
        rusqlite::params![
            &id,
            text,
            text_raw,
            language,
            model_id,
            source,
            duration_ms,
            word_count,
            &ts,
        ],
    )?;
    Ok(Transcript {
        id,
        text: text.to_string(),
        text_raw: text_raw.to_string(),
        language: language.map(String::from),
        model_id: model_id.to_string(),
        source: source.to_string(),
        duration_ms,
        word_count,
        created_at: ts,
        injected: false,
    })
}

pub fn list(db: &DbPool, limit: i64) -> Result<Vec<Transcript>> {
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT id,text,text_raw,language,model_id,source,
                duration_ms,word_count,created_at,injected
         FROM voice_transcripts
         ORDER BY created_at DESC
         LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit.max(1).min(500)], |r| {
        Ok(Transcript {
            id: r.get(0)?,
            text: r.get(1)?,
            text_raw: r.get(2)?,
            language: r.get(3)?,
            model_id: r.get(4)?,
            source: r.get(5)?,
            duration_ms: r.get(6)?,
            word_count: r.get(7)?,
            created_at: r.get(8)?,
            injected: r.get::<_, i64>(9)? != 0,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn search(db: &DbPool, query: &str, limit: i64) -> Result<Vec<Transcript>> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let q = sanitize_fts(query);
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT t.id,t.text,t.text_raw,t.language,t.model_id,t.source,
                t.duration_ms,t.word_count,t.created_at,t.injected
         FROM voice_transcripts_fts f
         JOIN voice_transcripts t ON t.rowid=f.rowid
         WHERE voice_transcripts_fts MATCH ?1
         ORDER BY bm25(voice_transcripts_fts)
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![&q, limit.max(1).min(100)], |r| {
        Ok(Transcript {
            id: r.get(0)?,
            text: r.get(1)?,
            text_raw: r.get(2)?,
            language: r.get(3)?,
            model_id: r.get(4)?,
            source: r.get(5)?,
            duration_ms: r.get(6)?,
            word_count: r.get(7)?,
            created_at: r.get(8)?,
            injected: r.get::<_, i64>(9)? != 0,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn delete(db: &DbPool, id: &str) -> Result<()> {
    let conn = db.get()?;
    conn.execute("DELETE FROM voice_transcripts WHERE id=?1", [id])?;
    Ok(())
}

pub fn stats(db: &DbPool, range: &str) -> Result<VoiceStats> {
    let cutoff = match range {
        "day" => Utc::now()
            .checked_sub_signed(chrono::Duration::days(1))
            .map(|d| d.to_rfc3339())
            .unwrap_or_default(),
        "week" => Utc::now()
            .checked_sub_signed(chrono::Duration::days(7))
            .map(|d| d.to_rfc3339())
            .unwrap_or_default(),
        "month" => Utc::now()
            .checked_sub_signed(chrono::Duration::days(30))
            .map(|d| d.to_rfc3339())
            .unwrap_or_default(),
        _ => "1970-01-01T00:00:00Z".to_string(),
    };
    let conn = db.get()?;
    // Clamp duration to >= 500ms when computing WPM to avoid noise from very
    // short transcripts dominating the average.
    let row = conn.query_row(
        "SELECT
           COUNT(*) AS sessions,
           COALESCE(SUM(word_count), 0) AS total_words,
           COALESCE(SUM(duration_ms), 0) / 1000.0 AS talk_seconds,
           CASE
             WHEN SUM(MAX(duration_ms, 500)) > 0
             THEN SUM(word_count) * 60000.0 / SUM(MAX(duration_ms, 500))
             ELSE 0.0
           END AS avg_wpm
         FROM voice_transcripts
         WHERE created_at >= ?1",
        [&cutoff],
        |r| {
            Ok(VoiceStats {
                sessions: r.get(0)?,
                total_words: r.get(1)?,
                talk_seconds: r.get(2)?,
                avg_wpm: r.get(3)?,
            })
        },
    )?;
    Ok(row)
}

pub fn export_jsonl(db: &DbPool, path: &std::path::Path) -> Result<usize> {
    use std::io::Write;
    let all = list(db, 100_000)?;
    let mut file = std::fs::File::create(path)?;
    let mut n = 0;
    for t in &all {
        let line = serde_json::to_string(t)?;
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        n += 1;
    }
    Ok(n)
}

fn sanitize_fts(q: &str) -> String {
    let cleaned: String = q
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' {
                c
            } else {
                ' '
            }
        })
        .collect();
    let toks: Vec<String> = cleaned
        .split_whitespace()
        .filter(|s| s.len() >= 2)
        .map(|s| s.to_lowercase())
        .collect();
    if toks.is_empty() {
        "x".to_string()
    } else {
        toks.join(" OR ")
    }
}

// We deliberately ignore an unused parameter — keep the signature stable in case
// callers move to scoped-by-id deletions later.
#[allow(dead_code)]
fn _unused() -> Result<()> {
    let _: Option<&str> = None;
    Ok(())
}

#[allow(dead_code)]
fn _silence(_: Error) {}
