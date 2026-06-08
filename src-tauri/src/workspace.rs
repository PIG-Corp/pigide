use crate::db::DbPool;
use crate::error::{Error, Result};
use crate::layout::LayoutNode;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::{Component, PathBuf};
use uuid::Uuid;

/// System roots a workspace path must never resolve to (or sit directly
/// under). Workspace paths become the file-access sandbox roots for the
/// `read_file` / `write_file` / `walk_files` commands, so a workspace
/// rooted at `/` or `/etc` would turn those commands into whole-disk
/// access. We reject the obvious escalation targets defensively.
const FORBIDDEN_ROOTS: &[&str] = &[
    "/", "/etc", "/bin", "/sbin", "/usr", "/boot", "/dev", "/proc", "/sys", "/root", "/var",
    "/lib", "/lib64",
];

/// Validate and canonicalise the workspace paths supplied by a caller.
///
/// Each path must be a non-empty, absolute, existing directory with no
/// `..`/`.` traversal segments. Symlinks are resolved via `canonicalize`
/// and the resolved target is re-checked, so a symlink pointing at a
/// forbidden root is rejected. Returns the canonicalised path strings.
pub fn validate_paths(paths: &[String]) -> Result<Vec<String>> {
    let mut out = Vec::with_capacity(paths.len());
    for raw in paths {
        out.push(validate_dir_path(raw)?);
    }
    Ok(out)
}

/// Validate and canonicalise a single directory path the way a workspace
/// root is validated: non-empty, absolute, no `..`/`.` segments, an existing
/// directory after symlink resolution, and not a protected system root.
/// Returns the canonicalised path string. Shared by workspace creation and
/// by commands that create files inside a caller-supplied directory
/// (project aliases, `mcp_register_cwd`).
pub fn validate_dir_path(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(Error::Invalid("path is empty".into()));
    }
    let p = PathBuf::from(trimmed);
    if !p.is_absolute() {
        return Err(Error::Invalid(format!(
            "path must be absolute: {}",
            trimmed
        )));
    }
    // Reject traversal segments before touching the filesystem.
    for comp in p.components() {
        if matches!(comp, Component::ParentDir | Component::CurDir) {
            return Err(Error::Invalid(format!(
                "path must not contain '..' or '.': {}",
                trimmed
            )));
        }
    }
    // Resolve symlinks + normalise. Must exist and be a directory.
    let canon = p
        .canonicalize()
        .map_err(|e| Error::Invalid(format!("path {}: {}", trimmed, e)))?;
    if !canon.is_dir() {
        return Err(Error::Invalid(format!(
            "path is not a directory: {}",
            trimmed
        )));
    }
    if is_forbidden_root(&canon) {
        return Err(Error::Invalid(format!(
            "path resolves to a protected system directory: {}",
            canon.display()
        )));
    }
    Ok(canon.to_string_lossy().to_string())
}

/// True when `canon` IS one of the forbidden system roots. We reject the
/// root itself; legitimate project dirs nested under e.g. `/usr/local/src`
/// are allowed since they are not the protected root directly.
fn is_forbidden_root(canon: &std::path::Path) -> bool {
    FORBIDDEN_ROOTS
        .iter()
        .any(|f| canon == std::path::Path::new(f))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub layout: LayoutNode,
    pub paths: Vec<String>,
    pub agent_count: usize,
}

pub struct WorkspaceManager {
    pub db: DbPool,
}

impl WorkspaceManager {
    pub fn new(db: DbPool) -> Self {
        Self { db }
    }

    pub fn list(&self) -> Result<Vec<Workspace>> {
        let conn = self.db.get()?;
        let mut stmt = conn.prepare(
            "SELECT w.id, w.name, w.created_at, w.layout_json, w.paths_json,
                    (SELECT COUNT(*) FROM agents a WHERE a.workspace_id=w.id AND a.status='running') as cnt
             FROM workspaces w
             ORDER BY w.created_at ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, i64>(5)? as usize,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, name, created_at, layout_json, paths_json, cnt) = row?;
            let layout: LayoutNode = serde_json::from_str(&layout_json).unwrap_or_default();
            let paths: Vec<String> = serde_json::from_str(&paths_json).unwrap_or_default();
            out.push(Workspace {
                id,
                name,
                created_at,
                layout,
                paths,
                agent_count: cnt,
            });
        }
        Ok(out)
    }

    pub fn get(&self, id: &str) -> Result<Workspace> {
        let conn = self.db.get()?;
        let mut stmt = conn.prepare(
            "SELECT id, name, created_at, layout_json, paths_json
             FROM workspaces WHERE id=?1",
        )?;
        let mut rows = stmt.query([id])?;
        let row = rows
            .next()?
            .ok_or_else(|| Error::NotFound(format!("workspace {}", id)))?;
        let id: String = row.get(0)?;
        let name: String = row.get(1)?;
        let created_at: String = row.get(2)?;
        let layout_json: String = row.get(3)?;
        let paths_json: String = row.get(4)?;
        let layout: LayoutNode = serde_json::from_str(&layout_json).unwrap_or_default();
        let paths: Vec<String> = serde_json::from_str(&paths_json).unwrap_or_default();
        let cnt: i64 = conn.query_row(
            "SELECT COUNT(*) FROM agents WHERE workspace_id=?1 AND status='running'",
            [&id],
            |r| r.get(0),
        )?;
        Ok(Workspace {
            id,
            name,
            created_at,
            layout,
            paths,
            agent_count: cnt as usize,
        })
    }

    pub fn create(&self, name: &str, paths: Vec<String>) -> Result<Workspace> {
        let paths = validate_paths(&paths)?;
        let id = Uuid::new_v4().to_string();
        let created_at = Utc::now().to_rfc3339();
        let layout = LayoutNode::Empty;
        let layout_json = serde_json::to_string(&layout)?;
        let paths_json = serde_json::to_string(&paths)?;
        let conn = self.db.get()?;
        conn.execute(
            "INSERT INTO workspaces(id,name,created_at,layout_json,paths_json)
             VALUES(?1,?2,?3,?4,?5)",
            [&id, name, &created_at, &layout_json, &paths_json],
        )?;
        Ok(Workspace {
            id,
            name: name.to_string(),
            created_at,
            layout,
            paths,
            agent_count: 0,
        })
    }

    pub fn rename(&self, id: &str, name: &str) -> Result<()> {
        let conn = self.db.get()?;
        let n = conn.execute("UPDATE workspaces SET name=?2 WHERE id=?1", [id, name])?;
        if n == 0 {
            return Err(Error::NotFound(format!("workspace {}", id)));
        }
        Ok(())
    }

    /// Remove leaves that point at agents which no longer exist. Idempotent.
    /// Fixes "blank xterm" when re-entering a workspace whose PTYs were killed.
    pub fn prune_stale_layout(&self, id: &str) -> Result<LayoutNode> {
        let mut ws = self.get(id)?;
        let conn = self.db.get()?;
        let mut stmt =
            conn.prepare("SELECT id FROM agents WHERE workspace_id=?1 AND status='running'")?;
        let live: std::collections::HashSet<String> = stmt
            .query_map([id], |r| r.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect();
        let leaves = ws.layout.leaves();
        let mut changed = false;
        for leaf in leaves {
            if !live.contains(&leaf) {
                let (next, removed) = std::mem::take(&mut ws.layout).remove_leaf(&leaf);
                ws.layout = next;
                if removed {
                    changed = true;
                }
            }
        }
        if changed {
            self.update_layout(id, &ws.layout)?;
        }
        Ok(ws.layout)
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        let conn = self.db.get()?;
        conn.execute("DELETE FROM workspaces WHERE id=?1", [id])?;
        Ok(())
    }

    pub fn update_layout(&self, id: &str, layout: &LayoutNode) -> Result<()> {
        let json = serde_json::to_string(layout)?;
        let conn = self.db.get()?;
        let n = conn.execute(
            "UPDATE workspaces SET layout_json=?2 WHERE id=?1",
            [id, &json],
        )?;
        if n == 0 {
            return Err(Error::NotFound(format!("workspace {}", id)));
        }
        Ok(())
    }

    pub fn set_paths(&self, id: &str, paths: Vec<String>) -> Result<()> {
        let paths = validate_paths(&paths)?;
        let json = serde_json::to_string(&paths)?;
        let conn = self.db.get()?;
        conn.execute(
            "UPDATE workspaces SET paths_json=?2 WHERE id=?1",
            [id, &json],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_paths_ok() {
        // Startup auto-creates a "default" workspace with no paths.
        assert_eq!(validate_paths(&[]).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn valid_existing_dir_is_canonicalised() {
        let tmp = std::env::temp_dir();
        let out = validate_paths(&[tmp.to_string_lossy().to_string()]).unwrap();
        assert_eq!(out.len(), 1);
        // Canonicalised result must itself be absolute + exist.
        assert!(std::path::Path::new(&out[0]).is_absolute());
        assert!(std::path::Path::new(&out[0]).is_dir());
    }

    #[test]
    fn rejects_relative_path() {
        let err = validate_paths(&["relative/dir".into()]).unwrap_err();
        assert!(matches!(err, Error::Invalid(_)));
    }

    #[test]
    fn rejects_parent_dir_traversal() {
        let tmp = std::env::temp_dir();
        let evil = format!("{}/../../etc", tmp.to_string_lossy());
        let err = validate_paths(&[evil]).unwrap_err();
        assert!(matches!(err, Error::Invalid(_)));
    }

    #[test]
    fn rejects_nonexistent_path() {
        let err = validate_paths(&["/nonexistent/pigide/xyzzy/12345".into()]).unwrap_err();
        assert!(matches!(err, Error::Invalid(_)));
    }

    #[test]
    fn rejects_forbidden_root() {
        // `/` and `/etc` are forbidden roots even though they exist.
        let err = validate_paths(&["/".into()]).unwrap_err();
        assert!(matches!(err, Error::Invalid(_)));
        if std::path::Path::new("/etc").is_dir() {
            let err = validate_paths(&["/etc".into()]).unwrap_err();
            assert!(matches!(err, Error::Invalid(_)));
        }
    }

    #[test]
    fn rejects_empty_string_path() {
        let err = validate_paths(&["   ".into()]).unwrap_err();
        assert!(matches!(err, Error::Invalid(_)));
    }

    #[test]
    fn validate_dir_path_canonicalises_and_rejects() {
        // Existing dir → canonical absolute path.
        let tmp = std::env::temp_dir();
        let ok = validate_dir_path(&tmp.to_string_lossy()).unwrap();
        assert!(std::path::Path::new(&ok).is_absolute());
        // Rejections mirror validate_paths.
        assert!(validate_dir_path("relative/x").is_err());
        assert!(validate_dir_path("/nonexistent/pigide/zzz/999").is_err());
        assert!(validate_dir_path("/").is_err());
        assert!(validate_dir_path("   ").is_err());
        let evil = format!("{}/../../etc", tmp.to_string_lossy());
        assert!(validate_dir_path(&evil).is_err());
    }
}
