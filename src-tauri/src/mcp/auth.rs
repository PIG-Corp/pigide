//! API key management for the MCP server.
//!
//! Keys are stored as SHA-256 hashes; the plaintext is shown only once at
//! creation time. `validate(presented)` returns the matching `KeyInfo` if any.

use crate::db::DbPool;
use crate::error::{Error, Result};
use chrono::Utc;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyInfo {
    pub id: String,
    pub label: String,
    pub scopes: Vec<String>,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreatedKey {
    pub info: KeyInfo,
    /// Plaintext key — present only at creation. Format `pk_<43 base64url>`.
    pub plaintext: String,
}

fn hash(plaintext: &str) -> String {
    let mut h = Sha256::new();
    h.update(plaintext.as_bytes());
    format!("{:x}", h.finalize())
}

/// Generate a new key. The plaintext is shown to the user once and only its
/// hash is persisted.
pub fn create(db: &DbPool, label: &str, scopes: Vec<String>) -> Result<CreatedKey> {
    if label.trim().is_empty() {
        return Err(Error::Invalid("label required".into()));
    }
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    use base64::Engine;
    let body = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let plaintext = format!("pk_{}", body);
    let id = Uuid::new_v4().to_string();
    let ts = Utc::now().to_rfc3339();
    let scope_str = if scopes.is_empty() {
        "read,mutate".to_string()
    } else {
        scopes.join(",")
    };
    let key_hash = hash(&plaintext);
    let conn = db.get()?;
    conn.execute(
        "INSERT INTO mcp_api_keys(id,label,key_hash,scopes,created_at,last_used_at)
         VALUES(?1,?2,?3,?4,?5,NULL)",
        rusqlite::params![&id, label, &key_hash, &scope_str, &ts],
    )?;
    Ok(CreatedKey {
        info: KeyInfo {
            id,
            label: label.to_string(),
            scopes: scope_str.split(',').map(String::from).collect(),
            created_at: ts,
            last_used_at: None,
        },
        plaintext,
    })
}

pub fn list(db: &DbPool) -> Result<Vec<KeyInfo>> {
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT id,label,scopes,created_at,last_used_at
         FROM mcp_api_keys ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map([], |r| {
        let scopes: String = r.get(2)?;
        Ok(KeyInfo {
            id: r.get(0)?,
            label: r.get(1)?,
            scopes: scopes.split(',').map(|s| s.trim().to_string()).collect(),
            created_at: r.get(3)?,
            last_used_at: r.get(4)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn revoke(db: &DbPool, id: &str) -> Result<()> {
    let conn = db.get()?;
    conn.execute("DELETE FROM mcp_api_keys WHERE id=?1", [id])?;
    Ok(())
}

/// Look up a presented plaintext key. On match, bumps `last_used_at`.
pub fn validate(db: &DbPool, presented: &str) -> Result<Option<KeyInfo>> {
    let h = hash(presented);
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT id,label,scopes,created_at,last_used_at
         FROM mcp_api_keys WHERE key_hash=?1 LIMIT 1",
    )?;
    let mut rows = stmt.query([&h])?;
    let row = match rows.next()? {
        Some(r) => r,
        None => return Ok(None),
    };
    let scopes_str: String = row.get(2)?;
    let info = KeyInfo {
        id: row.get(0)?,
        label: row.get(1)?,
        scopes: scopes_str.split(',').map(|s| s.trim().to_string()).collect(),
        created_at: row.get(3)?,
        last_used_at: row.get(4)?,
    };
    drop(rows);
    drop(stmt);
    let ts = Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE mcp_api_keys SET last_used_at=?2 WHERE id=?1",
        rusqlite::params![&info.id, &ts],
    )?;
    Ok(Some(info))
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
            "CREATE TABLE mcp_api_keys (
                id TEXT PRIMARY KEY, label TEXT NOT NULL, key_hash TEXT NOT NULL,
                scopes TEXT NOT NULL DEFAULT 'read,mutate',
                created_at TEXT NOT NULL, last_used_at TEXT);",
        )
        .unwrap();
        pool
    }

    #[test]
    fn create_validate_revoke() {
        let p = pool();
        let k = create(&p, "test", vec!["read".into()]).unwrap();
        let info = validate(&p, &k.plaintext).unwrap().unwrap();
        assert_eq!(info.label, "test");
        assert_eq!(info.scopes, vec!["read"]);
        revoke(&p, &k.info.id).unwrap();
        assert!(validate(&p, &k.plaintext).unwrap().is_none());
    }

    #[test]
    fn invalid_key_returns_none() {
        let p = pool();
        assert!(validate(&p, "pk_nope").unwrap().is_none());
    }
}
