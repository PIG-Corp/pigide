//! Phase 2 smart-lane worker. Drains `ingest_queue` per workspace,
//! sends batches to Haiku 4.5, applies returned upserts/edits.

use crate::db::DbPool;
use crate::error::Result;
use crate::memory::folders::Kind;
use crate::memory::ingest::prompt::{
    build_messages, parse_response, BatchItem, Edit, ExistingSlug, ParsedBatch, Upsert,
};
use crate::memory::ingest::queue::{mark_error, mark_processed, pending_for_workspace, QueueItem};
use crate::memory::note::IngestRecord;
use crate::memory::MemoryService;
use crate::orchestrator::client::OmniClient;
use crate::workspace::WorkspaceManager;
use chrono::Utc;
use std::sync::Arc;

pub const KEY_ENABLED: &str = "memory.smart_ingest.enabled";
pub const KEY_INTERVAL: &str = "memory.smart_ingest.interval_seconds";
pub const KEY_MODEL: &str = "memory.smart_ingest.model";
pub const KEY_MAX_NEW: &str = "memory.smart_ingest.max_notes_per_batch";
pub const KEY_WINDOW: &str = "memory.smart_ingest.batch_window_minutes";
pub const KEY_OMNI_BASE: &str = "omnirouter.base_url";

pub const DEFAULT_INTERVAL_SECS: u64 = 300;
pub const DEFAULT_MODEL: &str = "kr/claude-haiku-4-5-20251001";
pub const DEFAULT_MAX_NEW: usize = 5;
pub const DEFAULT_WINDOW_MINUTES: i64 = 30;
pub const DEFAULT_OMNI_BASE: &str = "http://localhost:20128";
pub const BATCH_SIZE: i64 = 8;
pub const MAX_BODY_BYTES: usize = 4096;
pub const MAX_EXISTING_SLUGS: usize = 50;

pub struct SmartIngestWorker {
    db: DbPool,
    memory: Arc<MemoryService>,
    ws_mgr: Arc<WorkspaceManager>,
}

impl SmartIngestWorker {
    pub fn new(db: DbPool, memory: Arc<MemoryService>, ws_mgr: Arc<WorkspaceManager>) -> Self {
        Self { db, memory, ws_mgr }
    }

    /// Spawn the tokio interval loop. Returns immediately.
    pub fn start(self: Arc<Self>) {
        tauri::async_runtime::spawn(async move {
            let mut last_period = self.interval_secs();
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(last_period));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            tick.tick().await; // consume the immediate first tick
            loop {
                tick.tick().await;
                if !self.enabled() {
                    continue;
                }
                let workspaces = match self.ws_mgr.list() {
                    Ok(w) => w,
                    Err(e) => {
                        tracing::warn!("smart-lane: list workspaces: {}", e);
                        continue;
                    }
                };
                for w in workspaces {
                    if let Err(e) = self.run_pass_for_workspace(&w.id).await {
                        tracing::warn!(workspace_id = %w.id, "smart-lane pass: {}", e);
                    }
                }
                // Re-read interval; if it changed, swap the timer for the next round.
                let cur_period = self.interval_secs();
                if cur_period != last_period {
                    last_period = cur_period;
                    tick = tokio::time::interval(std::time::Duration::from_secs(cur_period));
                    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                    tick.tick().await;
                }
            }
        });
    }

    /// One end-to-end pass for a single workspace. Public so tests can
    /// drive it without spinning up a tokio interval.
    pub async fn run_pass_for_workspace(&self, workspace_id: &str) -> Result<()> {
        let window = self.window_minutes();
        let pending = pending_for_workspace(&self.db, workspace_id, window, BATCH_SIZE)?;
        if pending.is_empty() {
            return Ok(());
        }
        let workspace_name = self
            .ws_mgr
            .get(workspace_id)
            .ok()
            .map(|w| w.name)
            .unwrap_or_else(|| workspace_id.to_string());
        let items = self.hydrate_items(&pending);
        let existing = self.collect_existing_slugs(workspace_id);
        let max_new = self.max_new();
        let messages = build_messages(&workspace_name, &items, &existing, max_new);
        let client = self.build_client();
        let resp = match client.chat_completions(messages, None).await {
            Ok(r) => r,
            Err(e) => {
                let ids: Vec<i64> = pending.iter().map(|p| p.id).collect();
                let _ = mark_error(&self.db, &ids, &format!("llm: {}", e));
                return Err(e);
            }
        };
        let text = resp
            .choices
            .first()
            .and_then(|c| c.message.content.clone())
            .unwrap_or_default();
        let parsed = match parse_response(&text) {
            Ok(p) => p,
            Err(e) => {
                let ids: Vec<i64> = pending.iter().map(|p| p.id).collect();
                let _ = mark_error(&self.db, &ids, &format!("parse: {}", e));
                return Err(e);
            }
        };
        self.apply_parsed(workspace_id, &pending, &parsed)?;
        let ids: Vec<i64> = pending.iter().map(|p| p.id).collect();
        mark_processed(&self.db, &ids)?;
        Ok(())
    }

    fn hydrate_items(&self, pending: &[QueueItem]) -> Vec<BatchItem> {
        let mut out = Vec::with_capacity(pending.len());
        for q in pending {
            let payload: serde_json::Value =
                serde_json::from_str(&q.payload_json).unwrap_or(serde_json::Value::Null);
            let note_id = payload
                .get("note_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if note_id.is_empty() {
                continue;
            }
            let note = match self.memory.get(note_id) {
                Ok(n) => n,
                Err(_) => continue,
            };
            let body = if note.body.len() > MAX_BODY_BYTES {
                let mut s = note.body.chars().take(MAX_BODY_BYTES).collect::<String>();
                s.push_str("\n…(truncated)…\n");
                s
            } else {
                note.body.clone()
            };
            out.push(BatchItem {
                queue_id: q.id,
                kind: q.kind.clone(),
                note_slug: note.slug,
                note_title: note.title,
                note_body: body,
            });
        }
        out
    }

    fn collect_existing_slugs(&self, workspace_id: &str) -> Vec<ExistingSlug> {
        let list = match self
            .memory
            .list(workspace_id, None, MAX_EXISTING_SLUGS as i64)
        {
            Ok(l) => l,
            Err(_) => return Vec::new(),
        };
        list.into_iter()
            .map(|n| ExistingSlug {
                slug: n.slug,
                title: n.title,
                kind: "source".into(), // NoteSummary doesn't carry kind today; default
            })
            .collect()
    }

    fn apply_parsed(
        &self,
        workspace_id: &str,
        pending: &[QueueItem],
        parsed: &ParsedBatch,
    ) -> Result<()> {
        let max_new = self.max_new();
        let upserts: &[Upsert] = if parsed.upsert.len() > max_new {
            &parsed.upsert[..max_new]
        } else {
            &parsed.upsert
        };
        // Track new-note slugs for backlinking.
        for u in upserts {
            self.apply_upsert(workspace_id, u)?;
        }
        for e in &parsed.edits {
            self.apply_edit(workspace_id, pending, e)?;
        }
        Ok(())
    }

    fn apply_upsert(&self, workspace_id: &str, u: &Upsert) -> Result<()> {
        let kind = Kind::parse(&u.kind).unwrap_or(Kind::Source);
        let title = u.title.trim();
        if title.is_empty() {
            return Ok(());
        }
        let slug = format!(
            "{}/{}",
            kind.folder(),
            crate::memory::storage::slugify(title)
        );
        let mut body = u.body.clone();
        if !u.links_to_slugs.is_empty() {
            body.push_str("\n\n## Related\n\n");
            for s in &u.links_to_slugs {
                body.push_str(&format!("- [[{}]]\n", s));
            }
        }
        let ingest = IngestRecord {
            source_kind: "smart_lane".into(),
            source_ref: None,
            ingested_at: Utc::now().to_rfc3339(),
            smart_pass_at: Some(Utc::now().to_rfc3339()),
        };
        self.memory.upsert_by_slug(
            workspace_id,
            &slug,
            title,
            &body,
            u.tags.clone(),
            kind,
            Some(ingest),
        )?;
        Ok(())
    }

    fn apply_edit(&self, workspace_id: &str, pending: &[QueueItem], e: &Edit) -> Result<()> {
        // Resolve the slug back to a note id by scanning our own pending
        // batch (cheaper than a workspace-wide search).
        let note_id = self.resolve_slug_to_id(workspace_id, &e.slug, pending);
        if let Some(id) = note_id {
            self.memory
                .append_section(&id, &e.append_section, &e.body)?;
        }
        Ok(())
    }

    fn resolve_slug_to_id(
        &self,
        workspace_id: &str,
        slug: &str,
        pending: &[QueueItem],
    ) -> Option<String> {
        // Fast path: check the pending batch's payloads for a note_id whose
        // slug matches.
        for q in pending {
            let p: serde_json::Value = serde_json::from_str(&q.payload_json).ok()?;
            let note_id = p.get("note_id").and_then(|v| v.as_str())?;
            let n = self.memory.get(note_id).ok()?;
            if n.slug == slug {
                return Some(n.id);
            }
        }
        // Fallback: look it up via list.
        let list = self.memory.list(workspace_id, None, 500).ok()?;
        list.into_iter().find(|n| n.slug == slug).map(|n| n.id)
    }

    // -------- settings shims --------

    fn enabled(&self) -> bool {
        crate::db::get_setting(&self.db, KEY_ENABLED)
            .ok()
            .flatten()
            .map(|v| v.to_ascii_lowercase() != "false")
            .unwrap_or(true)
    }

    fn interval_secs(&self) -> u64 {
        crate::db::get_setting(&self.db, KEY_INTERVAL)
            .ok()
            .flatten()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DEFAULT_INTERVAL_SECS)
            .max(10)
    }

    fn max_new(&self) -> usize {
        crate::db::get_setting(&self.db, KEY_MAX_NEW)
            .ok()
            .flatten()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(DEFAULT_MAX_NEW)
            .max(1)
    }

    fn window_minutes(&self) -> i64 {
        crate::db::get_setting(&self.db, KEY_WINDOW)
            .ok()
            .flatten()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(DEFAULT_WINDOW_MINUTES)
            .max(1)
    }

    fn model(&self) -> String {
        crate::db::get_setting(&self.db, KEY_MODEL)
            .ok()
            .flatten()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_MODEL.to_string())
    }

    fn omni_base(&self) -> String {
        crate::db::get_setting(&self.db, KEY_OMNI_BASE)
            .ok()
            .flatten()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_OMNI_BASE.to_string())
    }

    fn build_client(&self) -> OmniClient {
        OmniClient::new(self.omni_base(), self.model(), None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::ingest::queue;
    use r2d2_sqlite::SqliteConnectionManager;

    fn fresh_worker() -> (Arc<SmartIngestWorker>, String, std::path::PathBuf, DbPool) {
        let dir = std::env::temp_dir().join(format!("pigide-smart-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let manager = SqliteConnectionManager::file(dir.join("db.sqlite"));
        let pool = r2d2::Pool::builder()
            .max_size(4)
            .build(manager)
            .expect("pool");
        crate::db::migrate_one(&pool.get().unwrap()).unwrap();
        let ws_mgr = Arc::new(WorkspaceManager::new(pool.clone()));
        let ws = ws_mgr
            .create("smart-test", vec![dir.to_string_lossy().to_string()])
            .unwrap();
        let memory = Arc::new(MemoryService::new(pool.clone(), ws_mgr.clone()));
        let worker = Arc::new(SmartIngestWorker::new(pool.clone(), memory, ws_mgr));
        (worker, ws.id, dir, pool)
    }

    #[test]
    fn settings_defaults_apply_when_unset() {
        let (worker, _ws, dir, _db) = fresh_worker();
        assert!(worker.enabled());
        assert_eq!(worker.interval_secs(), DEFAULT_INTERVAL_SECS);
        assert_eq!(worker.max_new(), DEFAULT_MAX_NEW);
        assert_eq!(worker.window_minutes(), DEFAULT_WINDOW_MINUTES);
        assert_eq!(worker.model(), DEFAULT_MODEL);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn settings_override_via_db() {
        let (worker, _ws, dir, db) = fresh_worker();
        crate::db::set_setting(&db, KEY_INTERVAL, "60").unwrap();
        crate::db::set_setting(&db, KEY_MAX_NEW, "10").unwrap();
        crate::db::set_setting(&db, KEY_ENABLED, "false").unwrap();
        assert_eq!(worker.interval_secs(), 60);
        assert_eq!(worker.max_new(), 10);
        assert!(!worker.enabled());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn run_pass_no_op_when_queue_empty() {
        let (worker, ws, dir, _db) = fresh_worker();
        // Nothing in queue → returns Ok(()) without touching the network.
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(worker.run_pass_for_workspace(&ws)).unwrap();
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn apply_upsert_creates_concept_with_links() {
        let (worker, ws, dir, _db) = fresh_worker();
        let u = Upsert {
            kind: "concept".into(),
            title: "Idempotent upsert".into(),
            body: "Re-applying the same write is a no-op.".into(),
            tags: vec!["pattern".into()],
            links_to_slugs: vec!["tasks/abc-123".into()],
        };
        worker.apply_upsert(&ws, &u).unwrap();
        let list = worker.memory.list(&ws, None, 50).unwrap();
        let n = list
            .iter()
            .find(|x| x.slug == "concepts/idempotent-upsert")
            .expect("concept stored");
        let full = worker.memory.get(&n.id).unwrap();
        assert_eq!(full.kind, Kind::Concept);
        assert!(full.body.contains("[[tasks/abc-123]]"));
        assert!(full.tags.contains(&"pattern".to_string()));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn apply_edit_appends_section_to_existing_stub() {
        let (worker, ws, dir, _db) = fresh_worker();
        let stub = worker
            .memory
            .upsert_by_slug(
                &ws,
                "tasks/abc-123",
                "Task ABC",
                "## Summary\n\noriginal\n",
                vec![],
                Kind::Task,
                None,
            )
            .unwrap();
        // Make a fake pending row so resolve_slug_to_id finds it.
        let qid = queue::enqueue_task(&worker.db, &ws, "abc-123", &stub.id).unwrap();
        let pending = pending_for_workspace(&worker.db, &ws, 30, 50).unwrap();
        let e = Edit {
            slug: "tasks/abc-123".into(),
            append_section: "## Concepts referenced".into(),
            body: "- [[idempotent-upsert]]".into(),
        };
        worker.apply_edit(&ws, &pending, &e).unwrap();
        let after = worker.memory.get(&stub.id).unwrap();
        assert!(after.body.contains("## Concepts referenced"));
        assert!(after.body.contains("[[idempotent-upsert]]"));
        let _ = qid;
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn hydrate_items_truncates_long_bodies() {
        let (worker, ws, dir, _db) = fresh_worker();
        let big_body = "x".repeat(MAX_BODY_BYTES + 1000);
        let stub = worker
            .memory
            .upsert_by_slug(
                &ws,
                "tasks/big",
                "Big task",
                &big_body,
                vec![],
                Kind::Task,
                None,
            )
            .unwrap();
        queue::enqueue_task(&worker.db, &ws, "big-task", &stub.id).unwrap();
        let pending = pending_for_workspace(&worker.db, &ws, 30, 50).unwrap();
        let items = worker.hydrate_items(&pending);
        assert_eq!(items.len(), 1);
        assert!(items[0].note_body.len() <= MAX_BODY_BYTES + 50); // 50 byte truncation marker leeway
        assert!(items[0].note_body.ends_with("…(truncated)…\n"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn apply_parsed_caps_upserts_at_max_new() {
        let (worker, ws, dir, db) = fresh_worker();
        crate::db::set_setting(&db, KEY_MAX_NEW, "2").unwrap();
        let parsed = ParsedBatch {
            upsert: vec![
                Upsert {
                    kind: "concept".into(),
                    title: "C1".into(),
                    body: "b".into(),
                    tags: vec![],
                    links_to_slugs: vec![],
                },
                Upsert {
                    kind: "concept".into(),
                    title: "C2".into(),
                    body: "b".into(),
                    tags: vec![],
                    links_to_slugs: vec![],
                },
                Upsert {
                    kind: "concept".into(),
                    title: "C3".into(),
                    body: "b".into(),
                    tags: vec![],
                    links_to_slugs: vec![],
                },
            ],
            edits: vec![],
        };
        worker.apply_parsed(&ws, &[], &parsed).unwrap();
        let list = worker.memory.list(&ws, None, 50).unwrap();
        let concepts = list
            .iter()
            .filter(|n| n.slug.starts_with("concepts/"))
            .count();
        assert_eq!(concepts, 2);
        std::fs::remove_dir_all(dir).ok();
    }
}
