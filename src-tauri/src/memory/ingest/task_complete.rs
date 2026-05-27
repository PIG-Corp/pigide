//! Fast-lane writer triggered when a task transitions to `complete`.
//! Composes a `tasks/<task-id>.md` stub with title, instructions,
//! knowledge, agent, and files-touched. No LLM calls.

use crate::tasks::Task;

/// Files this task held a lock on at completion time. Pulled from
/// `swarm::ownership::list_for_task`. Empty when the task didn't claim
/// any files.
#[derive(Debug, Clone)]
pub struct FilesTouched {
    pub paths: Vec<String>,
}

/// Build the deterministic body for a task-complete stub. Pure function
/// for easy unit testing — caller is responsible for paths/IO.
pub fn render_task_body(task: &Task, files: &FilesTouched) -> String {
    let mut out = String::new();
    out.push_str("## Summary\n\n");
    if task.instructions.trim().is_empty() {
        out.push_str("_No instructions recorded._\n");
    } else {
        out.push_str(task.instructions.trim());
        out.push_str("\n");
    }

    if !task.knowledge.trim().is_empty() {
        out.push_str("\n## Knowledge\n\n");
        out.push_str(task.knowledge.trim());
        out.push_str("\n");
    }

    out.push_str("\n## Status\n\n");
    out.push_str(&format!("- Final status: `{}`\n", task.status));
    if let Some(agent) = &task.agent_id {
        out.push_str(&format!("- Assigned agent: `{}`\n", agent));
    }
    out.push_str(&format!("- Created: {}\n", task.created_at));
    out.push_str(&format!("- Updated: {}\n", task.updated_at));

    if !files.paths.is_empty() {
        out.push_str("\n## Files touched\n\n");
        for p in &files.paths {
            out.push_str(&format!("- `{}`\n", p));
        }
    }
    out
}

use crate::db::DbPool;
use crate::error::Result;
use crate::memory::folders::Kind;
use crate::memory::note::{IngestRecord, Note};
use crate::memory::MemoryService;
use crate::tasks::TaskManager;
use chrono::Utc;
use std::sync::Arc;
use tauri::AppHandle;

const FAST_INGEST_KEY: &str = "memory.fast_ingest.enabled";

fn fast_ingest_enabled(db: &DbPool) -> bool {
    crate::db::get_setting(db, FAST_INGEST_KEY)
        .ok()
        .flatten()
        .map(|v| v.to_ascii_lowercase() != "false")
        .unwrap_or(true)
}

/// Pure inner function: no AppHandle / no event emission. Returns the
/// `Note` written so callers can emit `memory://note.created`.
pub fn on_task_complete_inner(
    memory: &MemoryService,
    task_mgr: &TaskManager,
    db: &DbPool,
    workspace_id: &str,
    task_id: &str,
) -> Result<Note> {
    let task = task_mgr.get(task_id)?;
    let owners = crate::swarm::ownership::list_for_task(db, task_id).unwrap_or_default();
    let files = FilesTouched {
        paths: owners.into_iter().map(|o| o.path).collect(),
    };
    let body = render_task_body(&task, &files);
    let title = if task.title.trim().is_empty() {
        format!("Task {}", task_id)
    } else {
        task.title.clone()
    };
    let slug = format!("tasks/{}", task_id);
    let ingest = IngestRecord {
        source_kind: "task_complete".into(),
        source_ref: Some(task_id.to_string()),
        ingested_at: Utc::now().to_rfc3339(),
        smart_pass_at: None,
    };
    memory.upsert_by_slug(
        workspace_id,
        &slug,
        &title,
        &body,
        Vec::new(),
        Kind::Task,
        Some(ingest),
    )
}

/// Tauri-aware entry point. Honours the `memory.fast_ingest.enabled`
/// setting; failures log + swallow so they never roll back the user's
/// task transition.
pub fn on_task_complete(
    memory: Arc<MemoryService>,
    task_mgr: Arc<TaskManager>,
    db: DbPool,
    app: Option<AppHandle>,
    workspace_id: String,
    task_id: String,
) {
    if !fast_ingest_enabled(&db) {
        return;
    }
    let res = on_task_complete_inner(&memory, &task_mgr, &db, &workspace_id, &task_id);
    match res {
        Ok(note) => {
            if let Some(app) = app {
                super::events::emit_note_created(
                    &app,
                    &super::events::NoteCreatedPayload {
                        id: note.id.clone(),
                        slug: note.slug.clone(),
                        title: note.title.clone(),
                        kind: note.kind,
                        source_kind: "task_complete".into(),
                    },
                );
            }
        }
        Err(e) => {
            tracing::warn!(task_id = %task_id, "fast-lane task ingest failed: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::Task;

    fn sample_task() -> Task {
        Task {
            id: "abc-123".into(),
            workspace_id: "ws-1".into(),
            agent_id: Some("agent-1".into()),
            parent_id: None,
            title: "Wire ingest".into(),
            instructions: "Add task→complete hook.\n\nSecond paragraph.".into(),
            knowledge: "[[orchestrator]]".into(),
            status: "complete".into(),
            created_at: "2026-05-27T10:00:00Z".into(),
            updated_at: "2026-05-27T15:00:00Z".into(),
        }
    }

    #[test]
    fn renders_summary_knowledge_status_and_files() {
        let body = render_task_body(
            &sample_task(),
            &FilesTouched {
                paths: vec!["src/foo.rs".into(), "src/bar.rs".into()],
            },
        );
        assert!(body.contains("## Summary"));
        assert!(body.contains("Add task→complete hook"));
        assert!(body.contains("## Knowledge"));
        assert!(body.contains("[[orchestrator]]"));
        assert!(body.contains("## Status"));
        assert!(body.contains("- Final status: `complete`"));
        assert!(body.contains("- Assigned agent: `agent-1`"));
        assert!(body.contains("## Files touched"));
        assert!(body.contains("- `src/foo.rs`"));
        assert!(body.contains("- `src/bar.rs`"));
    }

    #[test]
    fn omits_optional_sections_when_empty() {
        let mut t = sample_task();
        t.knowledge = "".into();
        t.agent_id = None;
        let body = render_task_body(&t, &FilesTouched { paths: vec![] });
        assert!(!body.contains("## Knowledge"));
        assert!(!body.contains("## Files touched"));
        assert!(!body.contains("Assigned agent"));
        assert!(body.contains("## Summary"));
        assert!(body.contains("## Status"));
    }

    #[test]
    fn falls_back_to_placeholder_when_instructions_blank() {
        let mut t = sample_task();
        t.instructions = "  \n\t  ".into();
        let body = render_task_body(&t, &FilesTouched { paths: vec![] });
        assert!(body.contains("_No instructions recorded._"));
    }

    use crate::memory::folders::Kind;

    fn fresh_service_with_task() -> (
        std::sync::Arc<crate::memory::MemoryService>,
        std::sync::Arc<crate::tasks::TaskManager>,
        crate::db::DbPool,
        String,
        String,
        std::path::PathBuf,
    ) {
        let dir = std::env::temp_dir().join(format!("pigide-ingest-task-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let manager = r2d2_sqlite::SqliteConnectionManager::file(dir.join("db.sqlite"));
        let pool = r2d2::Pool::builder()
            .max_size(4)
            .build(manager)
            .expect("pool");
        crate::db::migrate_one(&pool.get().unwrap()).expect("migrate");
        let ws_mgr = std::sync::Arc::new(crate::workspace::WorkspaceManager::new(pool.clone()));
        let ws = ws_mgr
            .create("ingest-task", vec![dir.to_string_lossy().to_string()])
            .expect("create ws");
        let memory = std::sync::Arc::new(crate::memory::MemoryService::new(
            pool.clone(),
            ws_mgr.clone(),
        ));
        let task_mgr = std::sync::Arc::new(crate::tasks::TaskManager::new(pool.clone()));
        let task = task_mgr
            .create(crate::tasks::CreateTaskArgs {
                workspace_id: ws.id.clone(),
                title: "Wire ingest".into(),
                instructions: "Add hook.".into(),
                knowledge: "ref [[orchestrator]]".into(),
                parent_id: None,
            })
            .unwrap();
        (memory, task_mgr, pool, ws.id, task.id, dir)
    }

    #[test]
    fn on_task_complete_writes_stub_in_tasks_folder() {
        let (memory, task_mgr, db, ws_id, task_id, dir) = fresh_service_with_task();
        task_mgr
            .update(crate::tasks::UpdateTaskArgs {
                id: task_id.clone(),
                title: None,
                instructions: None,
                knowledge: None,
                agent_id: None,
                status: Some("complete".into()),
            })
            .unwrap();

        let result = on_task_complete_inner(&memory, &task_mgr, &db, &ws_id, &task_id).unwrap();

        assert_eq!(result.kind, Kind::Task);
        assert_eq!(result.slug, format!("tasks/{}", task_id));
        assert!(result.body.contains("## Summary"));
        assert!(result.body.contains("Add hook."));
        let ingest = result.ingest.expect("ingest set");
        assert_eq!(ingest.source_kind, "task_complete");
        assert_eq!(ingest.source_ref.as_deref(), Some(task_id.as_str()));
        assert!(ingest.smart_pass_at.is_none());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn on_task_complete_is_idempotent() {
        let (memory, task_mgr, db, ws_id, task_id, dir) = fresh_service_with_task();
        task_mgr
            .update(crate::tasks::UpdateTaskArgs {
                id: task_id.clone(),
                title: None,
                instructions: None,
                knowledge: None,
                agent_id: None,
                status: Some("complete".into()),
            })
            .unwrap();
        let n1 = on_task_complete_inner(&memory, &task_mgr, &db, &ws_id, &task_id).unwrap();
        let n2 = on_task_complete_inner(&memory, &task_mgr, &db, &ws_id, &task_id).unwrap();
        assert_eq!(n1.id, n2.id);
        assert_eq!(n2.slug, format!("tasks/{}", task_id));
        std::fs::remove_dir_all(dir).ok();
    }
}
