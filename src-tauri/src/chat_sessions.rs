//! Chat session management — list/create/rename/delete + current-session
//! pointer stored in `settings`.
//!
//! Two scopes:
//!   - `Global`    — `workspace_id IS NULL`. The classic chat that lives
//!                   across workspace switches. Pointer:
//!                   `current_session_id_global` (legacy
//!                   `current_session_id` is migrated on first read).
//!   - `Workspace` — `workspace_id = <ws_uuid>`. Lives only inside that
//!                   workspace; cascade-deleted with the workspace.
//!                   Pointer: `current_session_id_ws:<ws_uuid>`.
//!
//! Which one is "active" at any moment is governed by the `chat_scope`
//! setting (`"global"` | `"workspace"`). When `workspace`, we resolve via
//! `current_workspace_id` + the per-ws pointer. When that combination has
//! no pointer yet (or the pointed-to row is gone), `ensure_current` lazily
//! creates a default `"Main"` session in the right scope.

use crate::db::DbPool;
use crate::error::{Error, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatScope {
    Global,
    Workspace,
}

impl ChatScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            ChatScope::Global => "global",
            ChatScope::Workspace => "workspace",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "global" => Some(ChatScope::Global),
            "workspace" => Some(ChatScope::Workspace),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatSession {
    pub id: String,
    pub name: String,
    pub scope: ChatScope,
    /// Always `Some` when `scope == Workspace`, `None` when `Global`.
    pub workspace_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub message_count: i64,
}

/// Filter for `list`. `scope = None` → all sessions in any scope.
#[derive(Debug, Clone, Default)]
pub struct ListFilter {
    pub scope: Option<ChatScope>,
    /// Only meaningful when `scope == Some(ChatScope::Workspace)`.
    /// `None` = sessions of every workspace, `Some(id)` = only that one.
    pub workspace_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CurrentSession {
    pub id: String,
    pub scope: ChatScope,
    pub workspace_id: Option<String>,
}

fn row_to_session(r: &rusqlite::Row<'_>) -> rusqlite::Result<ChatSession> {
    let id: String = r.get(0)?;
    let name: String = r.get(1)?;
    let workspace_id: Option<String> = r.get(2)?;
    let created_at: String = r.get(3)?;
    let updated_at: String = r.get(4)?;
    let message_count: i64 = r.get(5)?;
    let scope = match workspace_id {
        Some(_) => ChatScope::Workspace,
        None => ChatScope::Global,
    };
    Ok(ChatSession {
        id,
        name,
        scope,
        workspace_id,
        created_at,
        updated_at,
        message_count,
    })
}

const SELECT_BASE: &str = "SELECT s.id, s.name, s.workspace_id, s.created_at, s.updated_at,
            (SELECT COUNT(*) FROM orchestrator_chat c WHERE c.session_id=s.id) AS cnt
     FROM chat_sessions s";

pub fn list(db: &DbPool, filter: &ListFilter) -> Result<Vec<ChatSession>> {
    let conn = db.get()?;
    let mut out = Vec::new();
    match (&filter.scope, &filter.workspace_id) {
        (None, _) => {
            let mut stmt =
                conn.prepare(&format!("{} ORDER BY s.updated_at DESC", SELECT_BASE))?;
            for row in stmt.query_map([], row_to_session)? {
                out.push(row?);
            }
        }
        (Some(ChatScope::Global), _) => {
            let mut stmt = conn.prepare(&format!(
                "{} WHERE s.workspace_id IS NULL ORDER BY s.updated_at DESC",
                SELECT_BASE
            ))?;
            for row in stmt.query_map([], row_to_session)? {
                out.push(row?);
            }
        }
        (Some(ChatScope::Workspace), Some(ws)) => {
            let mut stmt = conn.prepare(&format!(
                "{} WHERE s.workspace_id = ?1 ORDER BY s.updated_at DESC",
                SELECT_BASE
            ))?;
            for row in stmt.query_map([ws], row_to_session)? {
                out.push(row?);
            }
        }
        (Some(ChatScope::Workspace), None) => {
            let mut stmt = conn.prepare(&format!(
                "{} WHERE s.workspace_id IS NOT NULL ORDER BY s.updated_at DESC",
                SELECT_BASE
            ))?;
            for row in stmt.query_map([], row_to_session)? {
                out.push(row?);
            }
        }
    }
    Ok(out)
}

pub fn get(db: &DbPool, id: &str) -> Result<ChatSession> {
    let conn = db.get()?;
    let mut stmt = conn.prepare(&format!("{} WHERE s.id = ?1", SELECT_BASE))?;
    let mut rows = stmt.query([id])?;
    if let Some(row) = rows.next()? {
        row_to_session(row).map_err(Into::into)
    } else {
        Err(Error::NotFound(format!("session {}", id)))
    }
}

pub fn create(
    db: &DbPool,
    name: &str,
    scope: ChatScope,
    workspace_id: Option<&str>,
) -> Result<ChatSession> {
    if name.trim().is_empty() {
        return Err(Error::Invalid("session name required".into()));
    }
    let workspace_id = match (scope, workspace_id) {
        (ChatScope::Workspace, Some(id)) if !id.is_empty() => Some(id.to_string()),
        (ChatScope::Workspace, _) => {
            return Err(Error::Invalid(
                "workspace_id required for workspace-scoped session".into(),
            ));
        }
        (ChatScope::Global, _) => None,
    };
    let id = Uuid::new_v4().to_string();
    let ts = Utc::now().to_rfc3339();
    let conn = db.get()?;
    conn.execute(
        "INSERT INTO chat_sessions(id,name,workspace_id,created_at,updated_at)
         VALUES(?1,?2,?3,?4,?4)",
        rusqlite::params![&id, name, &workspace_id, &ts],
    )?;
    Ok(ChatSession {
        id,
        name: name.to_string(),
        scope,
        workspace_id,
        created_at: ts.clone(),
        updated_at: ts,
        message_count: 0,
    })
}

pub fn rename(db: &DbPool, id: &str, name: &str) -> Result<()> {
    let conn = db.get()?;
    let ts = Utc::now().to_rfc3339();
    let n = conn.execute(
        "UPDATE chat_sessions SET name=?2, updated_at=?3 WHERE id=?1",
        rusqlite::params![id, name, &ts],
    )?;
    if n == 0 {
        return Err(Error::NotFound(format!("session {}", id)));
    }
    Ok(())
}

pub fn delete(db: &DbPool, id: &str) -> Result<()> {
    let conn = db.get()?;
    conn.execute("DELETE FROM chat_sessions WHERE id=?1", [id])?;
    Ok(())
}

pub fn touch(db: &DbPool, id: &str) -> Result<()> {
    let conn = db.get()?;
    let ts = Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE chat_sessions SET updated_at=?2 WHERE id=?1",
        rusqlite::params![id, &ts],
    )?;
    Ok(())
}

// ---------- scope / pointers ----------

const SCOPE_KEY: &str = "chat_scope";
const POINTER_GLOBAL: &str = "current_session_id_global";
const POINTER_LEGACY: &str = "current_session_id";

fn pointer_ws(workspace_id: &str) -> String {
    format!("current_session_id_ws:{}", workspace_id)
}

pub fn get_scope(db: &DbPool) -> Result<ChatScope> {
    match crate::db::get_setting(db, SCOPE_KEY)? {
        Some(s) => Ok(ChatScope::parse(&s).unwrap_or(ChatScope::Global)),
        None => Ok(ChatScope::Global),
    }
}

pub fn set_scope(db: &DbPool, scope: ChatScope) -> Result<()> {
    crate::db::set_setting(db, SCOPE_KEY, scope.as_str())
}

pub fn current_workspace_id(db: &DbPool) -> Result<Option<String>> {
    Ok(crate::db::get_setting(db, "current_workspace_id")?
        .filter(|s| !s.is_empty()))
}

/// Resolve which (scope, workspace_id, pointer-key) tuple to use right now.
fn resolve_target(db: &DbPool) -> Result<(ChatScope, Option<String>, String)> {
    let scope = get_scope(db)?;
    match scope {
        ChatScope::Global => Ok((ChatScope::Global, None, POINTER_GLOBAL.to_string())),
        ChatScope::Workspace => match current_workspace_id(db)? {
            Some(ws) => {
                let key = pointer_ws(&ws);
                Ok((ChatScope::Workspace, Some(ws), key))
            }
            // Workspace scope requested but no current workspace — degrade
            // to global so the chat doesn't go dark.
            None => Ok((ChatScope::Global, None, POINTER_GLOBAL.to_string())),
        },
    }
}

/// Verify the pointer still references a session row that matches the
/// expected scope/workspace combination. Returns `None` if the pointer is
/// missing, empty, broken, or points at a row that no longer fits.
fn read_pointer(
    db: &DbPool,
    pointer_key: &str,
    scope: ChatScope,
    workspace_id: Option<&str>,
) -> Result<Option<String>> {
    let id = match crate::db::get_setting(db, pointer_key)? {
        Some(s) if !s.is_empty() => s,
        _ => return Ok(None),
    };
    let conn = db.get()?;
    let row: Option<Option<String>> = conn
        .query_row(
            "SELECT workspace_id FROM chat_sessions WHERE id=?1",
            [&id],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok();
    let ok = match (scope, row) {
        (ChatScope::Global, Some(None)) => true,
        (ChatScope::Workspace, Some(Some(ref ws))) => Some(ws.as_str()) == workspace_id,
        _ => false,
    };
    Ok(if ok { Some(id) } else { None })
}

/// Migrate the legacy single-pointer key to the new global-pointer key.
/// Idempotent: only runs when the new key is unset.
fn migrate_legacy_pointer(db: &DbPool) -> Result<()> {
    if crate::db::get_setting(db, POINTER_GLOBAL)?.is_some() {
        return Ok(());
    }
    if let Some(legacy) = crate::db::get_setting(db, POINTER_LEGACY)? {
        if !legacy.is_empty() {
            crate::db::set_setting(db, POINTER_GLOBAL, &legacy)?;
        }
    }
    Ok(())
}

/// Pick or create the right session for the active scope. Idempotent.
/// Prefers the most-recently-updated existing session in the scope before
/// minting a new "Main".
pub fn ensure_current(db: &DbPool) -> Result<String> {
    migrate_legacy_pointer(db)?;
    let (scope, ws, key) = resolve_target(db)?;
    if let Some(id) = read_pointer(db, &key, scope, ws.as_deref())? {
        return Ok(id);
    }
    // Pointer missing/stale — pick newest in scope or create.
    let filter = ListFilter {
        scope: Some(scope),
        workspace_id: ws.clone(),
    };
    let chosen = if let Some(s) = list(db, &filter)?.into_iter().next() {
        s.id
    } else {
        create(db, "Main", scope, ws.as_deref())?.id
    };
    crate::db::set_setting(db, &key, &chosen)?;
    Ok(chosen)
}

/// Public: full description of the active session. Resolves lazily, same
/// as `ensure_current`, but also returns scope+workspace_id for the UI.
pub fn current(db: &DbPool) -> Result<CurrentSession> {
    let id = ensure_current(db)?;
    let s = get(db, &id)?;
    Ok(CurrentSession {
        id: s.id,
        scope: s.scope,
        workspace_id: s.workspace_id,
    })
}

/// Mark `id` as current. Auto-detects scope from the session row, updates
/// `chat_scope` AND the matching pointer in one go.
pub fn set_current(db: &DbPool, id: &str) -> Result<CurrentSession> {
    let s = get(db, id)?;
    match s.scope {
        ChatScope::Global => {
            set_scope(db, ChatScope::Global)?;
            crate::db::set_setting(db, POINTER_GLOBAL, id)?;
        }
        ChatScope::Workspace => {
            let ws = s
                .workspace_id
                .as_deref()
                .ok_or_else(|| Error::Invalid("scoped session missing workspace_id".into()))?;
            set_scope(db, ChatScope::Workspace)?;
            crate::db::set_setting(db, &pointer_ws(ws), id)?;
        }
    }
    Ok(CurrentSession {
        id: s.id,
        scope: s.scope,
        workspace_id: s.workspace_id,
    })
}

/// Switch only the active scope; the per-scope pointer + ensure_current
/// resolves the rest.
pub fn switch_scope(db: &DbPool, scope: ChatScope) -> Result<CurrentSession> {
    set_scope(db, scope)?;
    current(db)
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
            "CREATE TABLE workspaces (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                created_at TEXT NOT NULL,
                layout_json TEXT NOT NULL DEFAULT '{}',
                paths_json TEXT NOT NULL DEFAULT '[]');
             CREATE TABLE chat_sessions (
                id          TEXT PRIMARY KEY,
                name        TEXT NOT NULL,
                workspace_id TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
                created_at  TEXT NOT NULL,
                updated_at  TEXT NOT NULL);
             CREATE TABLE orchestrator_chat (
                id              TEXT PRIMARY KEY,
                session_id      TEXT,
                role            TEXT NOT NULL,
                content         TEXT NOT NULL,
                tool_calls_json TEXT,
                tool_call_id    TEXT,
                created_at      TEXT NOT NULL);
             CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )
        .unwrap();
        pool
    }

    fn mk_ws(p: &DbPool, id: &str) {
        let conn = p.get().unwrap();
        conn.execute(
            "INSERT INTO workspaces(id,name,created_at) VALUES(?1,?2,?3)",
            rusqlite::params![id, "ws", "2026-01-01T00:00:00Z"],
        )
        .unwrap();
    }

    #[test]
    fn ensure_current_creates_global_main_when_empty() {
        let p = pool();
        let id = ensure_current(&p).unwrap();
        let s = get(&p, &id).unwrap();
        assert_eq!(s.scope, ChatScope::Global);
        assert!(s.workspace_id.is_none());
        assert_eq!(s.name, "Main");
    }

    #[test]
    fn ensure_current_creates_workspace_main_when_in_workspace_scope() {
        let p = pool();
        mk_ws(&p, "WS1");
        crate::db::set_setting(&p, "current_workspace_id", "WS1").unwrap();
        set_scope(&p, ChatScope::Workspace).unwrap();
        let id = ensure_current(&p).unwrap();
        let s = get(&p, &id).unwrap();
        assert_eq!(s.scope, ChatScope::Workspace);
        assert_eq!(s.workspace_id.as_deref(), Some("WS1"));
    }

    #[test]
    fn switching_workspace_returns_a_different_session() {
        let p = pool();
        mk_ws(&p, "WS1");
        mk_ws(&p, "WS2");
        set_scope(&p, ChatScope::Workspace).unwrap();
        crate::db::set_setting(&p, "current_workspace_id", "WS1").unwrap();
        let a = ensure_current(&p).unwrap();
        crate::db::set_setting(&p, "current_workspace_id", "WS2").unwrap();
        let b = ensure_current(&p).unwrap();
        assert_ne!(a, b);
        assert_eq!(get(&p, &a).unwrap().workspace_id.as_deref(), Some("WS1"));
        assert_eq!(get(&p, &b).unwrap().workspace_id.as_deref(), Some("WS2"));
    }

    #[test]
    fn global_pointer_separate_from_workspace_pointer() {
        let p = pool();
        mk_ws(&p, "WS1");
        // Default scope global → creates global Main.
        let g = ensure_current(&p).unwrap();
        // Switch to workspace → creates ws Main, NOT the global one.
        crate::db::set_setting(&p, "current_workspace_id", "WS1").unwrap();
        set_scope(&p, ChatScope::Workspace).unwrap();
        let w = ensure_current(&p).unwrap();
        assert_ne!(g, w);
        // Switch back to global → returns the original global pointer.
        set_scope(&p, ChatScope::Global).unwrap();
        let g2 = ensure_current(&p).unwrap();
        assert_eq!(g, g2);
    }

    #[test]
    fn list_filters_by_scope() {
        let p = pool();
        mk_ws(&p, "WS1");
        create(&p, "G1", ChatScope::Global, None).unwrap();
        create(&p, "W1", ChatScope::Workspace, Some("WS1")).unwrap();
        let global_only = list(
            &p,
            &ListFilter {
                scope: Some(ChatScope::Global),
                workspace_id: None,
            },
        )
        .unwrap();
        assert_eq!(global_only.len(), 1);
        assert_eq!(global_only[0].name, "G1");
        let ws_only = list(
            &p,
            &ListFilter {
                scope: Some(ChatScope::Workspace),
                workspace_id: Some("WS1".into()),
            },
        )
        .unwrap();
        assert_eq!(ws_only.len(), 1);
        assert_eq!(ws_only[0].name, "W1");
    }

    #[test]
    fn create_workspace_session_requires_workspace_id() {
        let p = pool();
        let err = create(&p, "x", ChatScope::Workspace, None).unwrap_err();
        assert!(matches!(err, Error::Invalid(_)));
    }

    #[test]
    fn set_current_auto_detects_scope() {
        let p = pool();
        mk_ws(&p, "WS1");
        let g = create(&p, "G", ChatScope::Global, None).unwrap();
        let w = create(&p, "W", ChatScope::Workspace, Some("WS1")).unwrap();
        let cur = set_current(&p, &w.id).unwrap();
        assert_eq!(cur.scope, ChatScope::Workspace);
        assert_eq!(get_scope(&p).unwrap(), ChatScope::Workspace);
        let cur = set_current(&p, &g.id).unwrap();
        assert_eq!(cur.scope, ChatScope::Global);
        assert_eq!(get_scope(&p).unwrap(), ChatScope::Global);
    }

    #[test]
    fn legacy_pointer_migrates_to_global_on_first_ensure() {
        let p = pool();
        let s = create(&p, "Old", ChatScope::Global, None).unwrap();
        crate::db::set_setting(&p, POINTER_LEGACY, &s.id).unwrap();
        let id = ensure_current(&p).unwrap();
        assert_eq!(id, s.id);
        let migrated = crate::db::get_setting(&p, POINTER_GLOBAL).unwrap();
        assert_eq!(migrated.as_deref(), Some(s.id.as_str()));
    }
}
