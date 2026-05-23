use crate::db::DbPool;
use crate::error::{Error, Result};
use crate::layout::LayoutNode;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
        let json = serde_json::to_string(&paths)?;
        let conn = self.db.get()?;
        conn.execute(
            "UPDATE workspaces SET paths_json=?2 WHERE id=?1",
            [id, &json],
        )?;
        Ok(())
    }
}
