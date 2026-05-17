//! Word-boundary regex replacements applied to Whisper output.
//!
//! Schema: `voice_dictionary(id, pattern, replacement, case_sense, enabled, created_at)`.
//! Compilation is cached per call (`Vec<(Regex, replacement)>`); UI flow is
//! expected to reload by calling `compile_all` after any mutation.

use crate::db::DbPool;
use crate::error::{Error, Result};
use chrono::Utc;
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DictEntry {
    pub id: String,
    pub pattern: String,
    pub replacement: String,
    pub case_sense: bool,
    pub enabled: bool,
    pub created_at: String,
}

pub fn list(db: &DbPool) -> Result<Vec<DictEntry>> {
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT id,pattern,replacement,case_sense,enabled,created_at
         FROM voice_dictionary ORDER BY length(pattern) DESC, created_at ASC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(DictEntry {
            id: r.get(0)?,
            pattern: r.get(1)?,
            replacement: r.get(2)?,
            case_sense: r.get::<_, i64>(3)? != 0,
            enabled: r.get::<_, i64>(4)? != 0,
            created_at: r.get(5)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn add(
    db: &DbPool,
    pattern: &str,
    replacement: &str,
    case_sense: bool,
) -> Result<DictEntry> {
    if pattern.trim().is_empty() {
        return Err(Error::Invalid("pattern required".into()));
    }
    let id = Uuid::new_v4().to_string();
    let ts = Utc::now().to_rfc3339();
    let conn = db.get()?;
    conn.execute(
        "INSERT INTO voice_dictionary(id,pattern,replacement,case_sense,enabled,created_at)
         VALUES(?1,?2,?3,?4,1,?5)",
        rusqlite::params![&id, pattern, replacement, case_sense as i64, &ts],
    )?;
    Ok(DictEntry {
        id,
        pattern: pattern.to_string(),
        replacement: replacement.to_string(),
        case_sense,
        enabled: true,
        created_at: ts,
    })
}

pub fn delete(db: &DbPool, id: &str) -> Result<()> {
    let conn = db.get()?;
    conn.execute("DELETE FROM voice_dictionary WHERE id=?1", [id])?;
    Ok(())
}

pub fn update(
    db: &DbPool,
    id: &str,
    pattern: Option<&str>,
    replacement: Option<&str>,
    case_sense: Option<bool>,
    enabled: Option<bool>,
) -> Result<()> {
    let conn = db.get()?;
    let cur = list(db)?
        .into_iter()
        .find(|e| e.id == id)
        .ok_or_else(|| Error::NotFound(format!("dict {}", id)))?;
    conn.execute(
        "UPDATE voice_dictionary
         SET pattern=?2, replacement=?3, case_sense=?4, enabled=?5
         WHERE id=?1",
        rusqlite::params![
            id,
            pattern.unwrap_or(&cur.pattern),
            replacement.unwrap_or(&cur.replacement),
            case_sense.unwrap_or(cur.case_sense) as i64,
            enabled.unwrap_or(cur.enabled) as i64
        ],
    )?;
    Ok(())
}

/// Apply all enabled entries to `input`. Substitutions run in insertion order
/// of `list()`, which is sorted by `length(pattern) DESC` so longer phrases
/// win over their substrings (`web hook` before `web`).
pub fn apply(db: &DbPool, input: &str) -> Result<String> {
    let entries = list(db)?;
    let mut out = input.to_string();
    for e in entries.into_iter().filter(|e| e.enabled) {
        let escaped = regex::escape(&e.pattern);
        let with_boundaries = format!(r"\b{}\b", escaped);
        let re: Regex = match RegexBuilder::new(&with_boundaries)
            .case_insensitive(!e.case_sense)
            .unicode(true)
            .build()
        {
            Ok(r) => r,
            Err(err) => {
                tracing::warn!("dict regex {:?} compile: {}", e.pattern, err);
                continue;
            }
        };
        out = re.replace_all(&out, e.replacement.as_str()).into_owned();
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool() -> DbPool {
        let manager = r2d2_sqlite::SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(2).build(manager).unwrap();
        let conn = pool.get().unwrap();
        conn.execute_batch(
            "CREATE TABLE voice_dictionary (
                id TEXT PRIMARY KEY,
                pattern TEXT NOT NULL,
                replacement TEXT NOT NULL,
                case_sense INTEGER NOT NULL DEFAULT 0,
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL);
             CREATE UNIQUE INDEX idx_voice_dict_pattern
                ON voice_dictionary(pattern, case_sense);",
        )
        .unwrap();
        pool
    }

    #[test]
    fn longer_match_wins() {
        let p = pool();
        add(&p, "web hook", "WebHook", false).unwrap();
        add(&p, "web", "Web", false).unwrap();
        let s = apply(&p, "this is a web hook for web design").unwrap();
        assert_eq!(s, "this is a WebHook for Web design");
    }

    #[test]
    fn case_insensitive_default() {
        let p = pool();
        add(&p, "typescript", "TypeScript", false).unwrap();
        let s = apply(&p, "I love TypeScript and typescript both").unwrap();
        assert_eq!(s, "I love TypeScript and TypeScript both");
    }

    #[test]
    fn boundaries_respected() {
        let p = pool();
        add(&p, "foo", "BAR", false).unwrap();
        let s = apply(&p, "foobar foo afoo").unwrap();
        assert_eq!(s, "foobar BAR afoo");
    }
}
