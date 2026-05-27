//! Hot cache builder — `meta/hot.md` reflects the most-recently-updated
//! concepts/entities/tasks and gets prepended to the orchestrator's system
//! prompt at chat-time. Run after every successful smart-lane pass so a
//! fresh chat starts with the right working set already in context.

use crate::error::Result;
use crate::memory::folders::Kind;
use crate::memory::note::IngestRecord;
use crate::memory::MemoryService;
use chrono::Utc;

const HOT_SLUG: &str = "meta/hot";
const HOT_TITLE: &str = "Hot — recent working set";
const TOP_N: usize = 8;

/// Rebuild `meta/hot.md` for `workspace_id`. Idempotent; safe to call
/// after every smart-lane tick.
pub fn rebuild(memory: &MemoryService, workspace_id: &str) -> Result<()> {
    let summaries = memory.list(workspace_id, None, 200)?;
    let recent: Vec<_> = summaries
        .into_iter()
        .filter(|n| {
            // Skip chat dumps (low signal) and the meta/hot itself.
            n.slug != HOT_SLUG && n.kind != Kind::Chat
        })
        .take(TOP_N)
        .collect();

    if recent.is_empty() {
        return Ok(());
    }

    let mut body = String::from(
        "_Auto-rebuilt by the smart-lane after each ingest pass. Top recently-touched notes the next chat session should know about._\n\n",
    );
    for n in &recent {
        body.push_str(&format!(
            "- [[{}]] — _{}_ ({})\n",
            n.slug,
            n.title.replace('\n', " "),
            n.kind.as_str()
        ));
    }

    let ingest = IngestRecord {
        source_kind: "hot_cache".into(),
        source_ref: None,
        ingested_at: Utc::now().to_rfc3339(),
        smart_pass_at: Some(Utc::now().to_rfc3339()),
    };
    memory.upsert_by_slug(
        workspace_id,
        HOT_SLUG,
        HOT_TITLE,
        &body,
        Vec::new(),
        Kind::Meta,
        Some(ingest),
    )?;
    Ok(())
}

/// Read the current `meta/hot.md` body for `workspace_id`. Returns
/// `None` when the file doesn't exist yet (no smart passes have run).
pub fn read_body(memory: &MemoryService, workspace_id: &str) -> Option<String> {
    let list = memory.list(workspace_id, None, 500).ok()?;
    let summary = list.into_iter().find(|n| n.slug == HOT_SLUG)?;
    let note = memory.get(&summary.id).ok()?;
    Some(note.body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn fresh() -> (Arc<MemoryService>, String, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("pigide-hot-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let manager = r2d2_sqlite::SqliteConnectionManager::file(dir.join("db.sqlite"));
        let pool = r2d2::Pool::builder().max_size(4).build(manager).unwrap();
        crate::db::migrate_one(&pool.get().unwrap()).unwrap();
        let ws_mgr = Arc::new(crate::workspace::WorkspaceManager::new(pool.clone()));
        let ws = ws_mgr
            .create("hot-test", vec![dir.to_string_lossy().to_string()])
            .unwrap();
        let memory = Arc::new(MemoryService::new(pool, ws_mgr));
        (memory, ws.id, dir)
    }

    #[test]
    fn rebuild_writes_meta_hot_with_links() {
        let (memory, ws, dir) = fresh();
        memory
            .upsert_by_slug(
                &ws,
                "concepts/idempotent",
                "Idempotent",
                "body",
                vec![],
                Kind::Concept,
                None,
            )
            .unwrap();
        memory
            .upsert_by_slug(
                &ws,
                "tasks/abc",
                "Task ABC",
                "body",
                vec![],
                Kind::Task,
                None,
            )
            .unwrap();
        rebuild(&memory, &ws).unwrap();
        let body = read_body(&memory, &ws).expect("hot.md exists");
        assert!(body.contains("[[concepts/idempotent]]"));
        assert!(body.contains("[[tasks/abc]]"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn rebuild_skips_chat_kind_and_self() {
        let (memory, ws, dir) = fresh();
        memory
            .upsert_by_slug(
                &ws,
                "chats/agent/2026-05-27",
                "Chat dump",
                "body",
                vec![],
                Kind::Chat,
                None,
            )
            .unwrap();
        memory
            .upsert_by_slug(&ws, "concepts/c", "C", "body", vec![], Kind::Concept, None)
            .unwrap();
        rebuild(&memory, &ws).unwrap();
        rebuild(&memory, &ws).unwrap(); // idempotent
        let body = read_body(&memory, &ws).expect("hot.md");
        assert!(!body.contains("chats/agent"));
        assert!(!body.contains("[[meta/hot]]"));
        assert!(body.contains("[[concepts/c]]"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn rebuild_no_op_when_workspace_empty() {
        let (memory, ws, dir) = fresh();
        rebuild(&memory, &ws).unwrap();
        assert!(read_body(&memory, &ws).is_none());
        std::fs::remove_dir_all(dir).ok();
    }
}
