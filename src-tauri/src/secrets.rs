//! Encrypted secret store for outbound API keys.
//!
//! Unlike MCP keys (which are SHA-256 hashed because they only need to be
//! *verified*), provider API keys must be *replayed* on every outbound
//! request, so they have to be recoverable. We therefore encrypt them at rest
//! with AES-256-GCM and persist only the ciphertext.
//!
//! - **Master key**: 32 random bytes in `<config_dir>/pigide/secret.key`,
//!   created on first use with `0600` permissions. Never logged.
//! - **At rest**: each secret is `base64(nonce ‖ ciphertext)` in the
//!   `secrets` KV table (separate from the plaintext `settings` table).
//! - **Logging**: secret values never touch `tracing`. Only key *names* and
//!   coarse success/failure are logged elsewhere.

use crate::db::DbPool;
use crate::error::{Error, Result};
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::Engine;
use rand::RngCore;
use std::path::PathBuf;
use std::sync::OnceLock;

const MASTER_KEY_FILE: &str = "secret.key";
const NONCE_LEN: usize = 12;

/// Process-wide cached master key (loaded once).
static MASTER_KEY: OnceLock<[u8; 32]> = OnceLock::new();

fn master_key_path() -> Result<PathBuf> {
    let base = dirs::config_dir().ok_or_else(|| Error::Other("config dir unavailable".into()))?;
    let dir = base.join("pigide");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join(MASTER_KEY_FILE))
}

/// Load the master key, generating + persisting it on first call.
fn master_key() -> Result<&'static [u8; 32]> {
    if let Some(k) = MASTER_KEY.get() {
        return Ok(k);
    }
    let key = load_or_create_master_key()?;
    // Ignore the race where another thread set it first — both derive the
    // same bytes from the same file, so either value is correct.
    let _ = MASTER_KEY.set(key);
    Ok(MASTER_KEY.get().expect("master key set"))
}

fn load_or_create_master_key() -> Result<[u8; 32]> {
    let path = master_key_path()?;
    if let Some(key) = read_master_key(&path)? {
        return Ok(key);
    }
    // First run: mint a fresh key. `create_new` makes this atomic across
    // concurrent callers — exactly one writer wins.
    let mut key = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut key);
    match write_master_key(&path, &key) {
        Ok(()) => Ok(key),
        Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // Another thread/process created it first. Re-read so every caller
            // converges on the same key instead of failing.
            read_master_key(&path)?
                .ok_or_else(|| Error::Other("secret.key vanished after creation race".into()))
        }
        Err(e) => Err(e),
    }
}

/// Read + validate the 32-byte master key, or `None` if the file is absent.
fn read_master_key(path: &PathBuf) -> Result<Option<[u8; 32]>> {
    match std::fs::read(path) {
        Ok(bytes) => {
            if bytes.len() != 32 {
                return Err(Error::Other(format!(
                    "secret.key has wrong length ({} != 32) — refusing to use",
                    bytes.len()
                )));
            }
            let mut key = [0u8; 32];
            key.copy_from_slice(&bytes);
            Ok(Some(key))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::Io(e)),
    }
}

#[cfg(unix)]
fn write_master_key(path: &PathBuf, key: &[u8; 32]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(key)?;
    f.flush()?;
    Ok(())
}

#[cfg(not(unix))]
fn write_master_key(path: &PathBuf, key: &[u8; 32]) -> Result<()> {
    use std::io::Write;
    // `create_new` keeps creation atomic across concurrent callers (the
    // AlreadyExists race is handled by the caller). The file lands in the
    // per-user config dir, which is already ACL-scoped to the user; POSIX
    // mode bits do not apply on this platform.
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    f.write_all(key)?;
    f.flush()?;
    Ok(())
}

fn cipher() -> Result<Aes256Gcm> {
    let key_bytes = master_key()?;
    let key = Key::<Aes256Gcm>::from_slice(key_bytes);
    Ok(Aes256Gcm::new(key))
}

/// Encrypt `plaintext` into `base64(nonce ‖ ciphertext)`.
fn encrypt(plaintext: &str) -> Result<String> {
    let cipher = cipher()?;
    let mut nonce_bytes = [0u8; NONCE_LEN];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ct = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|_| Error::Other("secret encryption failed".into()))?;
    let mut blob = Vec::with_capacity(NONCE_LEN + ct.len());
    blob.extend_from_slice(&nonce_bytes);
    blob.extend_from_slice(&ct);
    Ok(base64::engine::general_purpose::STANDARD.encode(blob))
}

/// Decrypt a `base64(nonce ‖ ciphertext)` blob back to plaintext.
fn decrypt(blob_b64: &str) -> Result<String> {
    let blob = base64::engine::general_purpose::STANDARD
        .decode(blob_b64.as_bytes())
        .map_err(|_| Error::Other("secret blob: invalid base64".into()))?;
    if blob.len() < NONCE_LEN + 1 {
        return Err(Error::Other("secret blob: truncated".into()));
    }
    let (nonce_bytes, ct) = blob.split_at(NONCE_LEN);
    let cipher = cipher()?;
    let nonce = Nonce::from_slice(nonce_bytes);
    let pt = cipher
        .decrypt(nonce, ct)
        .map_err(|_| Error::Other("secret decryption failed (wrong key or corrupt blob)".into()))?;
    String::from_utf8(pt).map_err(|_| Error::Other("secret: decrypted bytes are not UTF-8".into()))
}

fn ensure_table(pool: &DbPool) -> Result<()> {
    let conn = pool.get()?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS secrets (
            name  TEXT PRIMARY KEY,
            value TEXT NOT NULL
         )",
        [],
    )?;
    Ok(())
}

/// Store (or overwrite) a secret under `name`. The plaintext is encrypted
/// before it touches the database.
pub fn set_secret(pool: &DbPool, name: &str, plaintext: &str) -> Result<()> {
    ensure_table(pool)?;
    let blob = encrypt(plaintext)?;
    let conn = pool.get()?;
    conn.execute(
        "INSERT INTO secrets(name,value) VALUES(?1,?2)
         ON CONFLICT(name) DO UPDATE SET value=excluded.value",
        [name, &blob],
    )?;
    Ok(())
}

/// Fetch + decrypt a secret. Returns `None` if absent.
pub fn get_secret(pool: &DbPool, name: &str) -> Result<Option<String>> {
    ensure_table(pool)?;
    let conn = pool.get()?;
    let mut stmt = conn.prepare("SELECT value FROM secrets WHERE name = ?1")?;
    let mut rows = stmt.query([name])?;
    if let Some(row) = rows.next()? {
        let blob: String = row.get(0)?;
        Ok(Some(decrypt(&blob)?))
    } else {
        Ok(None)
    }
}

/// Whether a secret exists, without decrypting it. Used by the UI to render a
/// "key set" indicator without ever shipping the value to the webview.
pub fn has_secret(pool: &DbPool, name: &str) -> Result<bool> {
    ensure_table(pool)?;
    let conn = pool.get()?;
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM secrets WHERE name = ?1",
        [name],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

pub fn delete_secret(pool: &DbPool, name: &str) -> Result<()> {
    ensure_table(pool)?;
    let conn = pool.get()?;
    conn.execute("DELETE FROM secrets WHERE name = ?1", [name])?;
    Ok(())
}

/// Canonical secret name for a custom provider's API key.
pub fn provider_key_name(provider_id: &str) -> String {
    format!("provider.{}.api_key", provider_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_pool() -> DbPool {
        let manager = r2d2_sqlite::SqliteConnectionManager::memory();
        r2d2::Pool::builder().max_size(2).build(manager).unwrap()
    }

    #[test]
    fn roundtrip_encrypt_decrypt() {
        // Uses the real master key file in the config dir; fine for a local
        // test run. The plaintext should survive the round-trip.
        let secret = "sk-test-1234567890";
        let blob = encrypt(secret).unwrap();
        assert_ne!(blob, secret, "ciphertext must differ from plaintext");
        assert_eq!(decrypt(&blob).unwrap(), secret);
    }

    #[test]
    fn nonce_makes_ciphertext_unique() {
        let a = encrypt("same").unwrap();
        let b = encrypt("same").unwrap();
        assert_ne!(a, b, "random nonce must make repeated ciphertext differ");
    }

    #[test]
    fn store_fetch_delete() {
        let pool = temp_pool();
        assert!(!has_secret(&pool, "k").unwrap());
        set_secret(&pool, "k", "value-xyz").unwrap();
        assert!(has_secret(&pool, "k").unwrap());
        assert_eq!(get_secret(&pool, "k").unwrap().unwrap(), "value-xyz");
        delete_secret(&pool, "k").unwrap();
        assert!(!has_secret(&pool, "k").unwrap());
        assert!(get_secret(&pool, "k").unwrap().is_none());
    }

    #[test]
    fn db_stores_ciphertext_not_plaintext() {
        let pool = temp_pool();
        set_secret(&pool, "k", "plaintext-secret").unwrap();
        let conn = pool.get().unwrap();
        let raw: String = conn
            .query_row("SELECT value FROM secrets WHERE name='k'", [], |r| r.get(0))
            .unwrap();
        assert!(
            !raw.contains("plaintext-secret"),
            "raw DB value must not contain the plaintext"
        );
    }
}
