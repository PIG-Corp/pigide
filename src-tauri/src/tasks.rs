use crate::db::DbPool;
use crate::error::{Error, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Lifecycle states. Matches the CHECK constraint in the v4 migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Todo,
    InProgress,
    InReview,
    Complete,
    Cancelled,
}

impl TaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskStatus::Todo => "todo",
            TaskStatus::InProgress => "in_progress",
            TaskStatus::InReview => "in_review",
            TaskStatus::Complete => "complete",
            TaskStatus::Cancelled => "cancelled",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "todo" => Some(TaskStatus::Todo),
            "in_progress" => Some(TaskStatus::InProgress),
            "in_review" => Some(TaskStatus::InReview),
            "complete" => Some(TaskStatus::Complete),
            "cancelled" => Some(TaskStatus::Cancelled),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub workspace_id: String,
    pub agent_id: Option<String>,
    pub parent_id: Option<String>,
    pub title: String,
    pub instructions: String,
    pub knowledge: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateTaskArgs {
    pub workspace_id: String,
    pub title: String,
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub knowledge: String,
    #[serde(default)]
    pub parent_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateTaskArgs {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default)]
    pub knowledge: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub agent_id: Option<Option<String>>,
}

pub struct TaskManager {
    db: DbPool,
}

impl TaskManager {
    pub fn new(db: DbPool) -> Self {
        Self { db }
    }

    pub fn create(&self, args: CreateTaskArgs) -> Result<Task> {
        if args.title.trim().is_empty() {
            return Err(Error::Invalid("title required".into()));
        }
        // Verify workspace exists; we deliberately do not auto-create one.
        let conn = self.db.get()?;
        let exists: i64 = conn.query_row(
            "SELECT COUNT(*) FROM workspaces WHERE id=?1",
            [&args.workspace_id],
            |r| r.get(0),
        )?;
        if exists == 0 {
            return Err(Error::NotFound(format!("workspace {}", args.workspace_id)));
        }
        if let Some(parent) = &args.parent_id {
            let pe: i64 = conn.query_row(
                "SELECT COUNT(*) FROM tasks WHERE id=?1",
                [parent],
                |r| r.get(0),
            )?;
            if pe == 0 {
                return Err(Error::NotFound(format!("parent task {}", parent)));
            }
        }
        let id = Uuid::new_v4().to_string();
        let ts = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO tasks(id,workspace_id,agent_id,parent_id,title,instructions,knowledge,status,created_at,updated_at)
             VALUES(?1,?2,NULL,?3,?4,?5,?6,'todo',?7,?7)",
            rusqlite::params![
                &id,
                &args.workspace_id,
                &args.parent_id,
                &args.title,
                &args.instructions,
                &args.knowledge,
                &ts,
            ],
        )?;
        Ok(Task {
            id,
            workspace_id: args.workspace_id,
            agent_id: None,
            parent_id: args.parent_id,
            title: args.title,
            instructions: args.instructions,
            knowledge: args.knowledge,
            status: "todo".into(),
            created_at: ts.clone(),
            updated_at: ts,
        })
    }

    pub fn get(&self, id: &str) -> Result<Task> {
        let conn = self.db.get()?;
        let mut stmt = conn.prepare(
            "SELECT id,workspace_id,agent_id,parent_id,title,instructions,knowledge,status,created_at,updated_at
             FROM tasks WHERE id=?1",
        )?;
        let mut rows = stmt.query([id])?;
        let row = rows
            .next()?
            .ok_or_else(|| Error::NotFound(format!("task {}", id)))?;
        Ok(row_to_task(row)?)
    }

    pub fn list(
        &self,
        workspace_id: Option<&str>,
        status: Option<&str>,
        agent_id: Option<&str>,
    ) -> Result<Vec<Task>> {
        let conn = self.db.get()?;
        let mut sql = String::from(
            "SELECT id,workspace_id,agent_id,parent_id,title,instructions,knowledge,status,created_at,updated_at
             FROM tasks WHERE 1=1",
        );
        let mut params: Vec<String> = Vec::new();
        if let Some(w) = workspace_id {
            sql.push_str(&format!(" AND workspace_id=?{}", params.len() + 1));
            params.push(w.to_string());
        }
        if let Some(s) = status {
            if TaskStatus::parse(s).is_none() {
                return Err(Error::Invalid(format!("bad status: {}", s)));
            }
            sql.push_str(&format!(" AND status=?{}", params.len() + 1));
            params.push(s.to_string());
        }
        if let Some(a) = agent_id {
            sql.push_str(&format!(" AND agent_id=?{}", params.len() + 1));
            params.push(a.to_string());
        }
        sql.push_str(" ORDER BY created_at ASC");
        let mut stmt = conn.prepare(&sql)?;
        let refs: Vec<&dyn rusqlite::ToSql> =
            params.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
        let rows = stmt.query_map(refs.as_slice(), |r| {
            Ok(row_to_task(r).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
            }))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row??);
        }
        Ok(out)
    }

    pub fn update(&self, args: UpdateTaskArgs) -> Result<Task> {
        let conn = self.db.get()?;
        // Pull current state to assemble a partial update.
        let mut stmt = conn.prepare(
            "SELECT id,workspace_id,agent_id,parent_id,title,instructions,knowledge,status,created_at,updated_at
             FROM tasks WHERE id=?1",
        )?;
        let mut rows = stmt.query([&args.id])?;
        let row = rows
            .next()?
            .ok_or_else(|| Error::NotFound(format!("task {}", args.id)))?;
        let cur = row_to_task(row)?;
        drop(rows);
        drop(stmt);

        let title = args.title.unwrap_or(cur.title.clone());
        let instructions = args.instructions.unwrap_or(cur.instructions.clone());
        let knowledge = args.knowledge.unwrap_or(cur.knowledge.clone());
        let status = match args.status {
            Some(s) => {
                TaskStatus::parse(&s)
                    .ok_or_else(|| Error::Invalid(format!("bad status: {}", s)))?;
                // Review-gate enforcement: only let a task move to `complete`
                // when every gate on it has voted PASS. Builders/Coordinators
                // hit this when they try to close out work the Reviewer
                // hasn't signed off on. Other transitions (back to
                // in_progress, in_review, cancelled) are unrestricted.
                if s == "complete" && cur.status != "complete" {
                    crate::swarm::review::task_completable(&self.db, &args.id)?;
                }
                s
            }
            None => cur.status.clone(),
        };
        let agent_id = match args.agent_id {
            Some(opt) => opt,
            None => cur.agent_id.clone(),
        };
        let updated_at = Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE tasks
                SET title=?2, instructions=?3, knowledge=?4, status=?5,
                    agent_id=?6, updated_at=?7
              WHERE id=?1",
            rusqlite::params![
                &args.id,
                &title,
                &instructions,
                &knowledge,
                &status,
                &agent_id,
                &updated_at,
            ],
        )?;
        Ok(Task {
            id: cur.id,
            workspace_id: cur.workspace_id,
            agent_id,
            parent_id: cur.parent_id,
            title,
            instructions,
            knowledge,
            status,
            created_at: cur.created_at,
            updated_at,
        })
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        let conn = self.db.get()?;
        let n = conn.execute("DELETE FROM tasks WHERE id=?1", [id])?;
        if n == 0 {
            return Err(Error::NotFound(format!("task {}", id)));
        }
        // Drop any file locks the task was holding so they don't dangle.
        let _ = crate::swarm::ownership::release_all_for_task(&self.db, id);
        Ok(())
    }

    pub fn assign(&self, task_id: &str, agent_id: Option<&str>) -> Result<Task> {
        if let Some(aid) = agent_id {
            let conn = self.db.get()?;
            let exists: i64 = conn.query_row(
                "SELECT COUNT(*) FROM agents WHERE id=?1",
                [aid],
                |r| r.get(0),
            )?;
            if exists == 0 {
                return Err(Error::NotFound(format!("agent {}", aid)));
            }
        }
        self.update(UpdateTaskArgs {
            id: task_id.to_string(),
            title: None,
            instructions: None,
            knowledge: None,
            status: None,
            agent_id: Some(agent_id.map(|s| s.to_string())),
        })
    }
}

fn row_to_task(r: &rusqlite::Row) -> Result<Task> {
    Ok(Task {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        agent_id: r.get(2)?,
        parent_id: r.get(3)?,
        title: r.get(4)?,
        instructions: r.get(5)?,
        knowledge: r.get(6)?,
        status: r.get(7)?,
        created_at: r.get(8)?,
        updated_at: r.get(9)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool() -> DbPool {
        let manager = r2d2_sqlite::SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(2).build(manager).unwrap();
        let conn = pool.get().unwrap();
        conn.execute_batch(
            "CREATE TABLE workspaces (id TEXT PRIMARY KEY, name TEXT, created_at TEXT,
                                     layout_json TEXT DEFAULT '{}', paths_json TEXT DEFAULT '[]');
             CREATE TABLE agents (id TEXT PRIMARY KEY, workspace_id TEXT, type TEXT,
                                  cwd TEXT, status TEXT, created_at TEXT);
             CREATE TABLE tasks (id TEXT PRIMARY KEY,
                workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
                agent_id TEXT REFERENCES agents(id) ON DELETE SET NULL,
                parent_id TEXT REFERENCES tasks(id) ON DELETE SET NULL,
                title TEXT NOT NULL,
                instructions TEXT NOT NULL DEFAULT '',
                knowledge TEXT NOT NULL DEFAULT '',
                status TEXT NOT NULL DEFAULT 'todo'
                       CHECK(status IN ('todo','in_progress','in_review','complete','cancelled')),
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL);
             INSERT INTO workspaces(id,name,created_at) VALUES('w1','w1','2026-05-14');",
        )
        .unwrap();
        pool
    }

    #[test]
    fn create_then_update_status_transitions() {
        let mgr = TaskManager::new(pool());
        let t = mgr
            .create(CreateTaskArgs {
                workspace_id: "w1".into(),
                title: "do thing".into(),
                instructions: "details".into(),
                knowledge: String::new(),
                parent_id: None,
            })
            .unwrap();
        assert_eq!(t.status, "todo");
        let t2 = mgr
            .update(UpdateTaskArgs {
                id: t.id.clone(),
                title: None,
                instructions: None,
                knowledge: None,
                status: Some("in_progress".into()),
                agent_id: None,
            })
            .unwrap();
        assert_eq!(t2.status, "in_progress");
    }

    #[test]
    fn rejects_unknown_status() {
        let mgr = TaskManager::new(pool());
        let t = mgr
            .create(CreateTaskArgs {
                workspace_id: "w1".into(),
                title: "x".into(),
                instructions: String::new(),
                knowledge: String::new(),
                parent_id: None,
            })
            .unwrap();
        let err = mgr
            .update(UpdateTaskArgs {
                id: t.id,
                title: None,
                instructions: None,
                knowledge: None,
                status: Some("flying".into()),
                agent_id: None,
            })
            .unwrap_err();
        assert!(matches!(err, Error::Invalid(_)));
    }
}
