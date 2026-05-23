//! Local single-instance IPC server (#24).
//!
//! Listens on a Unix domain socket so a separately-installed `pigide-cli`
//! binary can hand a workspace path to an already-running PigIDE
//! instance — `pigide .` style — without having to know about Tauri or
//! shell out to dbus.
//!
//! Protocol: line-delimited JSON. One request per line, one response per
//! line. Closing the connection ends the session.

use crate::db::{self, DbPool};
use crate::workspace::WorkspaceManager;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Request {
    /// Health check.
    Ping,
    /// Open or create a workspace pinned to `path` and switch to it.
    OpenPath { path: String },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    Pong,
    Opened { workspace_id: String, name: String },
    Error { message: String },
}

/// Resolve the IPC socket path. We try `XDG_RUNTIME_DIR/pigide.sock` first
/// (per-user, auto-cleaned on logout) and fall back to /tmp.
pub fn socket_path() -> PathBuf {
    if let Ok(rt) = std::env::var("XDG_RUNTIME_DIR") {
        let p = PathBuf::from(rt);
        if p.is_dir() {
            return p.join("pigide.sock");
        }
    }
    let uid = std::env::var("UID").unwrap_or_else(|_| "default".into());
    PathBuf::from(format!("/tmp/pigide-{}.sock", uid))
}

#[cfg(unix)]
pub fn handle(req: Request, db: &DbPool) -> Response {
    match req {
        Request::Ping => Response::Pong,
        Request::OpenPath { path } => match open_path(db, &path) {
            Ok((id, name)) => Response::Opened {
                workspace_id: id,
                name,
            },
            Err(e) => Response::Error {
                message: e.to_string(),
            },
        },
    }
}

/// Resolve `path` to an absolute canonical form, then either reuse a
/// workspace already pinned to that directory or create a new one named
/// after its basename.
fn open_path(db: &DbPool, raw: &str) -> crate::error::Result<(String, String)> {
    let path = std::fs::canonicalize(Path::new(raw))?
        .to_string_lossy()
        .to_string();
    let ws_mgr = WorkspaceManager::new(db.clone());
    for w in ws_mgr.list()? {
        if w.paths.iter().any(|p| p == &path) {
            db::set_setting(db, "current_workspace_id", &w.id)?;
            return Ok((w.id, w.name));
        }
    }
    let name = Path::new(&path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "workspace".into());
    let ws = ws_mgr.create(&name, vec![path])?;
    db::set_setting(db, "current_workspace_id", &ws.id)?;
    Ok((ws.id, ws.name))
}

#[cfg(unix)]
fn handle_stream(mut stream: UnixStream, db: &DbPool) {
    let mut reader = BufReader::new(stream.try_clone().ok().unwrap_or_else(|| {
        // Best-effort fallback: if we can't dup, the writer below still works
        // because we own `stream`. This branch is unreachable in practice.
        stream.try_clone().expect("clone stream")
    }));
    let mut buf = String::new();
    while reader.read_line(&mut buf).map(|n| n > 0).unwrap_or(false) {
        let line = buf.trim().to_string();
        buf.clear();
        if line.is_empty() {
            continue;
        }
        let resp: Response = match serde_json::from_str::<Request>(&line) {
            Ok(req) => handle(req, db),
            Err(e) => Response::Error {
                message: format!("bad request: {}", e),
            },
        };
        let mut out = match serde_json::to_string(&resp) {
            Ok(s) => s,
            Err(e) => format!(r#"{{"kind":"error","message":"serialize: {}"}}"#, e),
        };
        out.push('\n');
        if stream.write_all(out.as_bytes()).is_err() {
            break;
        }
    }
}

/// Start the listener thread. Failures (port busy, permission denied) are
/// logged but never block app startup.
#[cfg(unix)]
pub fn spawn(db: DbPool) {
    let path = socket_path();
    if let Err(e) = std::fs::remove_file(&path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!("ipc socket {} cleanup: {}", path.display(), e);
        }
    }
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!("ipc bind {} failed: {}", path.display(), e);
            return;
        }
    };
    if let Err(e) = restrict_socket_permissions(&path) {
        tracing::warn!("ipc chmod {} failed: {}", path.display(), e);
        return;
    }
    tracing::info!("ipc listening on {}", path.display());
    let path_for_thread = path.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(s) => {
                    let db = db.clone();
                    std::thread::spawn(move || handle_stream(s, &db));
                }
                Err(e) => {
                    tracing::warn!("ipc accept on {}: {}", path_for_thread.display(), e);
                }
            }
        }
    });
}

#[cfg(unix)]
fn restrict_socket_permissions(path: &Path) -> std::io::Result<()> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
pub fn spawn(_db: DbPool) {
    tracing::info!("ipc socket: not supported on this platform");
}

#[cfg(test)]
mod tests {
    use super::*;
    use r2d2_sqlite::SqliteConnectionManager;
    use std::os::unix::fs::PermissionsExt;

    fn pool() -> DbPool {
        let manager = SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(2).build(manager).unwrap();
        let conn = pool.get().unwrap();
        conn.execute_batch(
            "CREATE TABLE workspaces (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                created_at TEXT NOT NULL,
                layout_json TEXT NOT NULL DEFAULT '{\"type\":\"empty\"}',
                paths_json TEXT NOT NULL DEFAULT '[]');
             CREATE TABLE agents (
                id TEXT PRIMARY KEY,
                workspace_id TEXT NOT NULL,
                type TEXT NOT NULL,
                cwd TEXT,
                status TEXT NOT NULL DEFAULT 'exited',
                created_at TEXT NOT NULL);
             CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )
        .unwrap();
        pool
    }

    #[test]
    fn ping_returns_pong() {
        let p = pool();
        match handle(Request::Ping, &p) {
            Response::Pong => {}
            other => panic!("got {:?}", other),
        }
    }

    #[test]
    fn open_path_creates_then_reuses_workspace() {
        let p = pool();
        let dir = tempdir_for_test("pigide-ipc-open");
        let raw = dir.to_string_lossy().to_string();

        // First call → creates a fresh workspace pinned to this path.
        let (id1, name1) = match handle(Request::OpenPath { path: raw.clone() }, &p) {
            Response::Opened { workspace_id, name } => (workspace_id, name),
            other => panic!("got {:?}", other),
        };
        assert!(!name1.is_empty());

        // Current workspace pointer should point at the new id.
        let cur = db::get_setting(&p, "current_workspace_id").unwrap();
        assert_eq!(cur.as_deref(), Some(id1.as_str()));

        // Second call → reuses the same workspace.
        let id2 = match handle(Request::OpenPath { path: raw }, &p) {
            Response::Opened { workspace_id, .. } => workspace_id,
            other => panic!("got {:?}", other),
        };
        assert_eq!(id2, id1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn open_path_rejects_nonexistent() {
        let p = pool();
        match handle(
            Request::OpenPath {
                path: "/this/should/not/exist/pigide-test".into(),
            },
            &p,
        ) {
            Response::Error { .. } => {}
            other => panic!("got {:?}", other),
        }
    }

    #[test]
    fn restrict_socket_permissions_sets_owner_only_mode() {
        let dir = tempdir_for_test("pigide-ipc-perms");
        let path = dir.join("sock");
        std::fs::write(&path, b"x").unwrap();
        restrict_socket_permissions(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        std::fs::remove_dir_all(dir).ok();
    }

    fn tempdir_for_test(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("{}-{}", label, uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
