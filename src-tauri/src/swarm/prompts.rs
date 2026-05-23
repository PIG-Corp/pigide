//! Per-workspace + per-role + per-agent-type system prompt overrides.
//!
//! BridgeSpace 3 lets a user supply custom system prompts for each agent
//! variant. PIG IDE stores those in the `role_prompts` table created by
//! migration v10. Lookup falls back from
//!   `(workspace, agent_type, role)` → `(workspace, "", role)` →
//!   `Role::default_prompt()`,
//! so a user can scope an override broadly (all coordinators in this
//! workspace) or narrowly (only `claude` coordinators in this workspace).

use crate::db::DbPool;
use crate::error::{Error, Result};
use crate::swarm::role::Role;
use chrono::Utc;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RolePromptOverride {
    pub workspace_id: String,
    pub agent_type: String,
    pub role: String,
    pub prompt: String,
    pub updated_at: String,
}

pub fn upsert(
    db: &DbPool,
    workspace_id: &str,
    agent_type: &str,
    role: &str,
    prompt: &str,
) -> Result<RolePromptOverride> {
    if Role::parse(role).is_none() {
        return Err(Error::Invalid(format!("unknown role: {}", role)));
    }
    if workspace_id.trim().is_empty() {
        return Err(Error::Invalid("workspace_id required".into()));
    }
    let ts = Utc::now().to_rfc3339();
    let conn = db.get()?;
    conn.execute(
        "INSERT INTO role_prompts(workspace_id, agent_type, role, prompt, updated_at)
         VALUES(?1,?2,?3,?4,?5)
         ON CONFLICT(workspace_id, agent_type, role)
         DO UPDATE SET prompt=excluded.prompt, updated_at=excluded.updated_at",
        rusqlite::params![workspace_id, agent_type, role, prompt, &ts],
    )?;
    Ok(RolePromptOverride {
        workspace_id: workspace_id.to_string(),
        agent_type: agent_type.to_string(),
        role: role.to_string(),
        prompt: prompt.to_string(),
        updated_at: ts,
    })
}

pub fn delete(db: &DbPool, workspace_id: &str, agent_type: &str, role: &str) -> Result<bool> {
    let conn = db.get()?;
    let n = conn.execute(
        "DELETE FROM role_prompts
         WHERE workspace_id=?1 AND agent_type=?2 AND role=?3",
        [workspace_id, agent_type, role],
    )?;
    Ok(n == 1)
}

pub fn list_for_workspace(db: &DbPool, workspace_id: &str) -> Result<Vec<RolePromptOverride>> {
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT workspace_id, agent_type, role, prompt, updated_at
         FROM role_prompts WHERE workspace_id=?1
         ORDER BY agent_type ASC, role ASC",
    )?;
    let rows = stmt.query_map([workspace_id], |r| {
        Ok(RolePromptOverride {
            workspace_id: r.get(0)?,
            agent_type: r.get(1)?,
            role: r.get(2)?,
            prompt: r.get(3)?,
            updated_at: r.get(4)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Resolve the effective system prompt for `(workspace, agent_type, role)`.
///
/// Lookup order:
///   1. exact workspace + agent_type + role
///   2. exact workspace + ""(any agent_type) + role
///   3. `Role::default_prompt()`
pub fn resolve(db: &DbPool, workspace_id: &str, agent_type: &str, role: Role) -> Result<String> {
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT prompt FROM role_prompts
         WHERE workspace_id=?1 AND agent_type=?2 AND role=?3",
    )?;
    if let Some(row) = stmt
        .query([workspace_id, agent_type, role.as_str()])?
        .next()?
    {
        return Ok(row.get::<_, String>(0)?);
    }
    let mut stmt = conn.prepare(
        "SELECT prompt FROM role_prompts
         WHERE workspace_id=?1 AND agent_type='' AND role=?2",
    )?;
    if let Some(row) = stmt.query([workspace_id, role.as_str()])?.next()? {
        return Ok(row.get::<_, String>(0)?);
    }
    Ok(role.default_prompt().to_string())
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
            "CREATE TABLE role_prompts (
                workspace_id TEXT NOT NULL,
                agent_type   TEXT NOT NULL DEFAULT '',
                role         TEXT NOT NULL,
                prompt       TEXT NOT NULL,
                updated_at   TEXT NOT NULL,
                PRIMARY KEY (workspace_id, agent_type, role));",
        )
        .unwrap();
        pool
    }

    #[test]
    fn upsert_then_resolve_exact() {
        let p = pool();
        upsert(&p, "w1", "claude", "coordinator", "be sharp").unwrap();
        assert_eq!(
            resolve(&p, "w1", "claude", Role::Coordinator).unwrap(),
            "be sharp"
        );
    }

    #[test]
    fn resolve_falls_back_to_workspace_wide() {
        let p = pool();
        upsert(&p, "w1", "", "builder", "build all the things").unwrap();
        assert_eq!(
            resolve(&p, "w1", "claude", Role::Builder).unwrap(),
            "build all the things"
        );
    }

    #[test]
    fn resolve_falls_back_to_role_default() {
        let p = pool();
        let prompt = resolve(&p, "w1", "claude", Role::Reviewer).unwrap();
        assert!(prompt.contains("Reviewer"));
    }

    #[test]
    fn upsert_replaces_existing_row() {
        let p = pool();
        upsert(&p, "w1", "", "builder", "v1").unwrap();
        upsert(&p, "w1", "", "builder", "v2").unwrap();
        let all = list_for_workspace(&p, "w1").unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].prompt, "v2");
    }

    #[test]
    fn delete_only_matching_row() {
        let p = pool();
        upsert(&p, "w1", "", "builder", "x").unwrap();
        upsert(&p, "w1", "claude", "builder", "y").unwrap();
        assert!(delete(&p, "w1", "", "builder").unwrap());
        let rest = list_for_workspace(&p, "w1").unwrap();
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].agent_type, "claude");
    }

    #[test]
    fn upsert_rejects_unknown_role() {
        let p = pool();
        let err = upsert(&p, "w1", "", "ghost", "no").unwrap_err();
        assert!(matches!(err, Error::Invalid(_)));
    }
}
