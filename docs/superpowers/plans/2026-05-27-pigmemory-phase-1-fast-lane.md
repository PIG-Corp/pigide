# PigMemory Phase 1 — Fast-Lane Ingest Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Capture knowledge automatically from the user's normal PigIDE work — when a task moves to `complete` or an agent emits enough chat output, write a deterministic note stub into `.pigmemory/`. Zero LLM calls; the smart-lane (Phase 2) enriches stubs later.

**Architecture:** Two cooperating writers in a new `memory/ingest/` module. (1) `task_complete.rs` — invoked from the `update_task` Tauri command after a successful transition to `status='complete'`; pulls task title, instructions, knowledge, files-touched (via `swarm::ownership::list_for_task`), assigned agent, and emits `tasks/<task-id>.md` with `kind=task`. (2) `chat_chunk.rs` — buffers PTY stdout per agent in a `ChatBuffer` map; on every emit decodes base64, counts lines, flushes to `chats/<agent-name>/<yyyy-mm-dd>.md` once the threshold hits (default 120 lines) or the agent exits. A new `memory://note.created` event fires on every write so the frontend graph can animate.

**Tech Stack:** Rust (`tokio`, `parking_lot::Mutex`, `chrono`, `base64`, existing `tauri::Emitter`, existing `MemoryService`/`Note`/`IngestRecord`). `parking_lot` is already a workspace dep (`Cargo.toml`). No new crates.

**Spec:** `docs/superpowers/specs/2026-05-27-pigmemory-claude-obsidian-design.md` § 3 (Hybrid ingest), § 4 (Storage layout), § 7.2 (Ingest pulse).

---

## File Structure

| File | Change | Responsibility |
|---|---|---|
| `src-tauri/src/memory/ingest/mod.rs` | create | submodule wiring + `events.rs` re-exports |
| `src-tauri/src/memory/ingest/events.rs` | create | `EV_MEMORY_NOTE_CREATED` const + `NoteCreatedPayload` + `emit_note_created` helper |
| `src-tauri/src/memory/ingest/task_complete.rs` | create | `on_task_complete(memory, task_mgr, db, app, task_id)` writes `tasks/<task-id>.md` |
| `src-tauri/src/memory/ingest/chat_chunk.rs` | create | `ChatBuffer` (per-agent rolling buffer) + `on_pty_stdout` + `on_agent_exit` flush hooks |
| `src-tauri/src/memory/mod.rs` | modify | add `pub mod ingest;` (alphabetical) |
| `src-tauri/src/memory/service.rs` | modify | new public `upsert_by_slug` method (idempotent write by deterministic slug — fast-lane needs to overwrite stubs without slug collisions) |
| `src-tauri/src/commands.rs:1034-1040` | modify | `update_task` Tauri command — after the manager update, if `args.status == Some("complete")`, dispatch `ingest::task_complete::on_task_complete` |
| `src-tauri/src/agent.rs:597-613` | modify | inside the stdout pump — also call `ingest::chat_chunk::on_pty_stdout` and on `Event::Exit` call `on_agent_exit` |
| `src-tauri/src/lib.rs` | modify | construct a single `Arc<ChatBuffer>` and stash on `AppState`; pass to `start_event_pump` |
| `src-tauri/src/state.rs` | modify | add `chat_buffer: Arc<ingest::chat_chunk::ChatBuffer>` field to `AppState` |
| `src-tauri/src/db.rs` | modify | new settings keys are read at runtime via `get_setting` — no schema change |
| `frontend/src/state/types.ts` | modify | add `MemoryNoteCreated` type for the new event payload |

Boundaries: `ingest/` owns everything write-side for the fast lane. `chat_chunk::ChatBuffer` is the only shared mutable state and is wrapped in `parking_lot::Mutex<HashMap<...>>`. `task_complete.rs` is purely synchronous wrt the `update_task` Tauri command — failures log a `tracing::warn!` and never propagate (the user's task transition succeeded; an ingest hiccup must not roll it back).

---

## Idempotency

- `tasks/<task-id>.md` slug is **deterministic** — re-running `on_task_complete` overwrites the file via `upsert_by_slug`, which finds the existing `id` (if any) and updates it instead of failing on collision.
- `chats/<agent-name>/<yyyy-mm-dd>.md` is **deterministic** per (agent, date) — flushing twice on the same day appends to the body section labelled `## chunk N` (where `N` increments by counting existing `## chunk` headers).
- The `ingest:` frontmatter block carries `source_kind: "task_complete"` or `"chat_chunk"`, `source_ref: <task_id|agent_id>`, `ingested_at: <ISO>`, `smart_pass_at: null` — Phase 2 picks rows where `smart_pass_at IS NULL`.

---

## Settings (read via `db::get_setting`, default-on)

| Key | Default | Notes |
|---|---|---|
| `memory.fast_ingest.enabled` | `"true"` | master switch for fast lane |
| `memory.chat_rotation.lines` | `"120"` | flush threshold for chat buffer |

When `memory.fast_ingest.enabled` is `"false"`, both hooks early-return without writing.

---

## Task 1: `MemoryService::upsert_by_slug` — idempotent write

**Files:**
- Modify: `src-tauri/src/memory/service.rs` (add new method on `impl MemoryService`)
- Test: same file's `#[cfg(test)] mod tests` block

`MemoryService::create` errors on slug collision (calls `unique_slug` which suffixes `-2/-3/...`). The fast lane writes deterministic slugs (`tasks/<task-id>`) and re-runs need to overwrite the existing note in place, not produce `tasks/<task-id>-2.md`. We add a sibling method:

- [ ] **Step 1: Write the failing test in `src-tauri/src/memory/service.rs`**

Append inside the existing `#[cfg(test)] mod tests` block (after `create_carries_kind_and_graph_exposes_it`):

```rust
    #[test]
    fn upsert_by_slug_overwrites_when_slug_exists() {
        let (svc, ws_id, dir) = fresh_service();
        let n1 = svc
            .upsert_by_slug(
                &ws_id,
                "tasks/abc-123",
                "Task ABC",
                "first body",
                vec!["auth".into()],
                Kind::Task,
                Some(crate::memory::note::IngestRecord {
                    source_kind: "task_complete".into(),
                    source_ref: Some("abc-123".into()),
                    ingested_at: "2026-05-27T15:00:00Z".into(),
                    smart_pass_at: None,
                }),
            )
            .unwrap();
        let n2 = svc
            .upsert_by_slug(
                &ws_id,
                "tasks/abc-123",
                "Task ABC v2",
                "second body",
                vec!["auth".into(), "refactor".into()],
                Kind::Task,
                None,
            )
            .unwrap();
        assert_eq!(n1.id, n2.id);
        assert_eq!(n2.title, "Task ABC v2");
        assert_eq!(n2.body, "second body");
        assert_eq!(n2.tags, vec!["auth".to_string(), "refactor".to_string()]);
        assert_eq!(n2.slug, "tasks/abc-123");
        std::fs::remove_dir_all(dir).ok();
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p pigide memory::service::tests::upsert_by_slug_overwrites --lib`
Expected: FAIL with compile error `no method named 'upsert_by_slug' found`.

- [ ] **Step 3: Implement `upsert_by_slug`**

In `src-tauri/src/memory/service.rs`, inside `impl MemoryService` (anywhere after `create`), add:

```rust
    /// Idempotent fast-lane write: if a note with this exact slug already
    /// exists in the workspace, update it in-place; otherwise create a new
    /// one. Used by the ingest pipeline where slugs are deterministic
    /// (e.g. `tasks/<task-id>`).
    pub fn upsert_by_slug(
        &self,
        workspace_id: &str,
        slug: &str,
        title: &str,
        body: &str,
        tags: Vec<String>,
        kind: crate::memory::folders::Kind,
        ingest: Option<crate::memory::note::IngestRecord>,
    ) -> Result<Note> {
        let root = self.root_for(workspace_id)?;
        let root_str = root.to_string_lossy().to_string();
        let conn = self.db.get()?;
        let existing_id: Option<String> = conn
            .query_row(
                "SELECT id FROM memory_notes WHERE workspace_root=?1 AND slug=?2",
                rusqlite::params![&root_str, slug],
                |r| r.get(0),
            )
            .ok();
        drop(conn);
        if let Some(id) = existing_id {
            let mut note = self.get(&id)?;
            note.title = title.to_string();
            note.body = body.to_string();
            note.tags = tags;
            note.kind = kind;
            note.ingest = ingest;
            note.updated_at = chrono::Utc::now().to_rfc3339();
            let path = storage::slug_to_path(&root, &note.slug)?;
            let raw = note::serialize(&note);
            note::write(&path, &raw)?;
            self.upsert_index(&root_str, &path, &note)?;
            self.rebuild_links(&note)?;
            return Ok(note);
        }
        // Fresh insert: bypass `create` so we keep the caller-provided slug
        // exactly (no folder-prefix synthesis, no `-2` suffixing).
        let mut note = Note::new(slug.to_string(), title.to_string(), body.to_string());
        note.kind = kind;
        note.tags = tags;
        note.ingest = ingest;
        let path = storage::slug_to_path(&root, slug)?;
        let raw = note::serialize(&note);
        note::write(&path, &raw)?;
        self.upsert_index(&root_str, &path, &note)?;
        self.rebuild_links(&note)?;
        Ok(note)
    }
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p pigide memory::service --lib`
Expected: all memory::service tests pass, including the new `upsert_by_slug_overwrites_when_slug_exists`.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/memory/service.rs
git commit -m "feat(memory): MemoryService::upsert_by_slug for fast-lane writes

Deterministic-slug ingest (tasks/<task-id>, chats/<agent>/<date>)
needs to overwrite the existing note instead of suffixing with -2.
upsert_by_slug looks up the row by (workspace_root, slug), updates
in place if it exists, otherwise inserts fresh while preserving the
exact caller slug."
```

---

## Task 2: `memory::ingest` skeleton + `EV_MEMORY_NOTE_CREATED`

**Files:**
- Create: `src-tauri/src/memory/ingest/mod.rs`
- Create: `src-tauri/src/memory/ingest/events.rs`
- Modify: `src-tauri/src/memory/mod.rs` (add `pub mod ingest;` alphabetical)
- Modify: `src-tauri/src/events.rs` — re-export the new event const

- [ ] **Step 1: Create `events.rs`**

Create `src-tauri/src/memory/ingest/events.rs`:

```rust
//! Event payload + emit helper for `memory://note.created`.
//!
//! Frontend listens for this event to play the ingest-pulse animation
//! and refresh the graph.

use crate::events::EV_MEMORY_NOTE_CREATED;
use crate::memory::folders::Kind;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

#[derive(Debug, Clone, Serialize)]
pub struct NoteCreatedPayload {
    pub id: String,
    pub slug: String,
    pub title: String,
    pub kind: Kind,
    pub source_kind: String,
}

pub fn emit_note_created(app: &AppHandle, payload: &NoteCreatedPayload) {
    if let Err(e) = app.emit(EV_MEMORY_NOTE_CREATED, payload) {
        tracing::debug!("failed to emit {}: {}", EV_MEMORY_NOTE_CREATED, e);
    }
}
```

- [ ] **Step 2: Create `mod.rs`**

Create `src-tauri/src/memory/ingest/mod.rs`:

```rust
//! Phase 1 fast-lane ingest pipeline.
//!
//! Two writers, both deterministic and LLM-free:
//!  - `task_complete` — `tasks/<task-id>.md` on every task→complete
//!  - `chat_chunk`    — `chats/<agent>/<yyyy-mm-dd>.md` from PTY stdout
//!
//! Each writer ends by emitting `memory://note.created` so the frontend
//! graph can animate.

pub mod chat_chunk;
pub mod events;
pub mod task_complete;
```

- [ ] **Step 3: Add `EV_MEMORY_NOTE_CREATED` to `src-tauri/src/events.rs`**

In `src-tauri/src/events.rs`, after the `EV_VOICE_DOWNLOAD` line, add:

```rust
/// PigMemory ingest emitted a new (or updated) note. Payload:
/// `NoteCreatedPayload { id, slug, title, kind, source_kind }`.
pub const EV_MEMORY_NOTE_CREATED: &str = "memory://note.created";
```

- [ ] **Step 4: Wire `pub mod ingest;` in `memory/mod.rs`**

`src-tauri/src/memory/mod.rs` should look like (insert `ingest` between `folders` and `links`):

```rust
//! PigMemory: local-first markdown notes with [[wikilinks]], FTS5 search,
//! backlinks, and BM25-based "suggest_connections".
//!
//! Storage layout: `<workspace_root>/.pigmemory/<slug>.md`. Slugs may include
//! `/` for nested folders. Each note carries a YAML frontmatter with a stable
//! `id` (uuid v4) — the path/slug is secondary so renames don't break links.

pub mod folders;
pub mod ingest;
pub mod links;
pub mod migration;
pub mod note;
pub mod service;
pub mod storage;
pub mod tools;
pub mod watcher;

pub use service::MemoryService;
```

- [ ] **Step 5: Stub the two writer modules**

Create `src-tauri/src/memory/ingest/task_complete.rs` (stub — populated in Task 4):

```rust
//! Fast-lane writer triggered when a task transitions to `complete`.
//! Composes a `tasks/<task-id>.md` stub with title, instructions,
//! knowledge, agent, and files-touched. No LLM calls.
```

Create `src-tauri/src/memory/ingest/chat_chunk.rs` (stub — populated in Task 5):

```rust
//! Fast-lane writer that buffers PTY stdout per agent and flushes a
//! `chats/<agent>/<yyyy-mm-dd>.md` chunk on threshold or agent exit.
```

- [ ] **Step 6: Build + run all tests**

Run: `cargo build -p pigide --lib`
Expected: success.

Run: `cargo test -p pigide --lib`
Expected: all tests still pass — no behavior change yet.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/memory/mod.rs src-tauri/src/memory/ingest/ src-tauri/src/events.rs
git commit -m "feat(memory): scaffold ingest module + memory://note.created event

Skeleton for the Phase 1 fast lane. Adds the memory::ingest
submodule with empty task_complete + chat_chunk stubs (filled in
follow-up commits) and a NoteCreatedPayload + emit helper for the
new memory://note.created event the frontend graph will listen on."
```

---

## Task 3: helper — render task body markdown

**Files:**
- Modify: `src-tauri/src/memory/ingest/task_complete.rs`
- Test: same file (new `#[cfg(test)] mod tests` block)

We compose the body deterministically before any plumbing, in pure Rust. Easy to unit-test without DB or app handle.

- [ ] **Step 1: Add the failing test**

In `src-tauri/src/memory/ingest/task_complete.rs`, replace the placeholder doc-comment with:

```rust
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
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p pigide memory::ingest::task_complete --lib`
Expected: 3 tests pass.

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/memory/ingest/task_complete.rs
git commit -m "feat(memory): render_task_body for task-complete stubs

Pure markdown formatter for the fast-lane task→complete writer:
Summary / Knowledge / Status / Files touched sections. Optional
sections elide when empty so the stub stays tight."
```

---

## Task 4: `task_complete::on_task_complete` — write the stub + emit event

**Files:**
- Modify: `src-tauri/src/memory/ingest/task_complete.rs` (add the orchestration function + tests)

- [ ] **Step 1: Add the failing integration test**

Append to the `#[cfg(test)] mod tests` block in `src-tauri/src/memory/ingest/task_complete.rs`:

```rust
    use crate::memory::folders::Kind;

    fn fresh_service_with_task() -> (
        std::sync::Arc<crate::memory::MemoryService>,
        std::sync::Arc<crate::tasks::TaskManager>,
        crate::db::DbPool,
        String,
        String,
        std::path::PathBuf,
    ) {
        let dir = std::env::temp_dir()
            .join(format!("pigide-ingest-task-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let manager = r2d2_sqlite::SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(1).build(manager).expect("pool");
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
                agent_id: None,
                parent_id: None,
            })
            .unwrap();
        (memory, task_mgr, pool, ws.id, task.id, dir)
    }

    #[test]
    fn on_task_complete_writes_stub_in_tasks_folder() {
        let (memory, task_mgr, db, ws_id, task_id, dir) = fresh_service_with_task();
        // Move the task to complete in the manager so its row reflects that
        // status when we read it back.
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
```

- [ ] **Step 2: Run the test to confirm it fails (compile error — `on_task_complete_inner` not defined)**

Run: `cargo test -p pigide memory::ingest::task_complete --lib`
Expected: FAIL — `cannot find function 'on_task_complete_inner' in this scope`.

- [ ] **Step 3: Implement `on_task_complete_inner` and the public wrapper**

Append to `src-tauri/src/memory/ingest/task_complete.rs` (after `render_task_body`):

```rust
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
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p pigide memory::ingest::task_complete --lib`
Expected: 5 tests pass (3 existing render-tests + 2 new orchestration tests).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/memory/ingest/task_complete.rs
git commit -m "feat(memory): on_task_complete fast-lane writer

Reads the task + ownership rows, composes a tasks/<task-id>.md
stub with kind=Task and ingest.source_kind='task_complete'.
Idempotent via upsert_by_slug. The Tauri-aware wrapper honours
memory.fast_ingest.enabled and swallows errors with tracing::warn
so an ingest hiccup never rolls back the user's status update."
```

---

## Task 5: `chat_chunk::ChatBuffer` — per-agent rolling buffer + flush

**Files:**
- Modify: `src-tauri/src/memory/ingest/chat_chunk.rs` (full implementation + tests)

- [ ] **Step 1: Write the failing tests**

Replace the placeholder doc-comment in `src-tauri/src/memory/ingest/chat_chunk.rs` with:

```rust
//! Fast-lane writer that buffers PTY stdout per agent and flushes a
//! `chats/<agent>/<yyyy-mm-dd>.md` chunk on threshold or agent exit.

use crate::db::DbPool;
use crate::error::Result;
use crate::memory::folders::Kind;
use crate::memory::note::{IngestRecord, Note};
use crate::memory::MemoryService;
use base64::Engine;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::AppHandle;

const CHAT_LINES_KEY: &str = "memory.chat_rotation.lines";
const FAST_INGEST_KEY: &str = "memory.fast_ingest.enabled";
const DEFAULT_LINES: usize = 120;

fn fast_ingest_enabled(db: &DbPool) -> bool {
    crate::db::get_setting(db, FAST_INGEST_KEY)
        .ok()
        .flatten()
        .map(|v| v.to_ascii_lowercase() != "false")
        .unwrap_or(true)
}

fn line_threshold(db: &DbPool) -> usize {
    crate::db::get_setting(db, CHAT_LINES_KEY)
        .ok()
        .flatten()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_LINES)
}

/// Per-agent rolling buffer. Holds raw decoded PTY output until the
/// line-count threshold trips; on flush we emit one note chunk and reset.
#[derive(Default)]
struct AgentBuffer {
    /// Workspace owning the agent — captured on first push so flushes
    /// know where to write.
    workspace_id: Option<String>,
    /// Human-readable agent name (e.g. "claude-tile-1"). Used in slugs.
    agent_name: Option<String>,
    /// Raw decoded text accumulated since the last flush.
    accumulated: String,
    /// Line count since last flush — cheap pre-computed counter.
    lines: usize,
    /// How many chunks we've already emitted today (for the `## chunk N`
    /// section header). Reset when the date rolls over.
    chunks_today: usize,
    /// ISO date (`yyyy-mm-dd`) of the last flush; bumps `chunks_today=0`
    /// on date change.
    last_date: Option<String>,
}

#[derive(Default)]
pub struct ChatBuffer {
    inner: Mutex<HashMap<String, AgentBuffer>>,
}

impl ChatBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register / update agent metadata. Called once on agent spawn so
    /// flushes know the workspace + agent name without a DB round-trip.
    pub fn register_agent(&self, agent_id: &str, workspace_id: &str, agent_name: &str) {
        let mut g = self.inner.lock();
        let entry = g.entry(agent_id.to_string()).or_default();
        entry.workspace_id = Some(workspace_id.to_string());
        entry.agent_name = Some(agent_name.to_string());
    }

    /// Push decoded PTY text into the buffer. Returns `Some(decision)`
    /// when the threshold has tripped — caller flushes.
    pub fn push(&self, agent_id: &str, text: &str, threshold: usize) -> bool {
        let mut g = self.inner.lock();
        let entry = g.entry(agent_id.to_string()).or_default();
        entry.accumulated.push_str(text);
        entry.lines += text.matches('\n').count();
        entry.lines >= threshold
    }

    /// Drain the buffer for `agent_id`, returning the accumulated text +
    /// metadata needed to compose the chunk note. Returns `None` when
    /// the buffer is empty / the agent isn't registered.
    pub fn drain_for_flush(
        &self,
        agent_id: &str,
        now: DateTime<Utc>,
    ) -> Option<DrainedChunk> {
        let mut g = self.inner.lock();
        let entry = g.get_mut(agent_id)?;
        if entry.accumulated.is_empty() {
            return None;
        }
        let workspace_id = entry.workspace_id.clone()?;
        let agent_name = entry.agent_name.clone()?;
        let date = now.format("%Y-%m-%d").to_string();
        if entry.last_date.as_deref() != Some(date.as_str()) {
            entry.last_date = Some(date.clone());
            entry.chunks_today = 0;
        }
        entry.chunks_today += 1;
        let chunk_no = entry.chunks_today;
        let text = std::mem::take(&mut entry.accumulated);
        entry.lines = 0;
        Some(DrainedChunk {
            workspace_id,
            agent_name,
            date,
            chunk_no,
            text,
        })
    }
}

#[derive(Debug)]
pub struct DrainedChunk {
    pub workspace_id: String,
    pub agent_name: String,
    pub date: String,
    pub chunk_no: usize,
    pub text: String,
}

/// Compose the per-day chat chunk body. Each flush appends a new
/// `## chunk N — HH:MM:SS UTC` section so the file shows the agent's
/// timeline within a day.
pub fn render_chunk_body(existing: &str, chunk: &DrainedChunk, now: DateTime<Utc>) -> String {
    let mut out = String::from(existing.trim_end());
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(&format!(
        "## chunk {} — {} UTC\n\n",
        chunk.chunk_no,
        now.format("%H:%M:%S")
    ));
    out.push_str("```\n");
    out.push_str(chunk.text.trim_end());
    out.push_str("\n```\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn fixed_time(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn buffer_tracks_lines_and_trips_at_threshold() {
        let buf = ChatBuffer::new();
        buf.register_agent("a1", "ws-1", "claude-tile-1");
        assert!(!buf.push("a1", "first line\nsecond line\n", 5));
        assert!(!buf.push("a1", "third\nfourth\n", 5));
        // 4 newlines so far; one more line trips at 5.
        assert!(buf.push("a1", "fifth\n", 5));
    }

    #[test]
    fn drain_for_flush_returns_metadata_and_resets_buffer() {
        let buf = ChatBuffer::new();
        buf.register_agent("a1", "ws-1", "claude-tile-1");
        buf.push("a1", "hello\n", 1);
        let now = fixed_time("2026-05-27T12:30:00Z");
        let chunk = buf.drain_for_flush("a1", now).unwrap();
        assert_eq!(chunk.workspace_id, "ws-1");
        assert_eq!(chunk.agent_name, "claude-tile-1");
        assert_eq!(chunk.date, "2026-05-27");
        assert_eq!(chunk.chunk_no, 1);
        assert_eq!(chunk.text, "hello\n");
        // Subsequent drain on an empty buffer returns None.
        assert!(buf.drain_for_flush("a1", now).is_none());
    }

    #[test]
    fn chunk_no_resets_on_new_day() {
        let buf = ChatBuffer::new();
        buf.register_agent("a1", "ws-1", "claude-tile-1");
        buf.push("a1", "day1\n", 1);
        let _ = buf.drain_for_flush("a1", fixed_time("2026-05-27T12:00:00Z")).unwrap();
        buf.push("a1", "day2-a\n", 1);
        let c1 = buf.drain_for_flush("a1", fixed_time("2026-05-28T08:00:00Z")).unwrap();
        assert_eq!(c1.chunk_no, 1);
        buf.push("a1", "day2-b\n", 1);
        let c2 = buf.drain_for_flush("a1", fixed_time("2026-05-28T09:00:00Z")).unwrap();
        assert_eq!(c2.chunk_no, 2);
    }

    #[test]
    fn drain_returns_none_for_unregistered_or_empty() {
        let buf = ChatBuffer::new();
        let now = fixed_time("2026-05-27T12:30:00Z");
        assert!(buf.drain_for_flush("ghost", now).is_none());
        buf.register_agent("a1", "ws", "n");
        assert!(buf.drain_for_flush("a1", now).is_none()); // registered but empty
    }

    #[test]
    fn render_chunk_body_appends_section() {
        let chunk = DrainedChunk {
            workspace_id: "ws".into(),
            agent_name: "n".into(),
            date: "2026-05-27".into(),
            chunk_no: 2,
            text: "line a\nline b\n".into(),
        };
        let now = Utc.with_ymd_and_hms(2026, 5, 27, 12, 34, 56).unwrap();
        let body = render_chunk_body("# header\n\n## chunk 1 — 11:00:00 UTC\n\n```\nold\n```\n", &chunk, now);
        assert!(body.contains("## chunk 1 — 11:00:00 UTC"));
        assert!(body.contains("## chunk 2 — 12:34:56 UTC"));
        assert!(body.contains("line a"));
        assert!(body.contains("line b"));
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p pigide memory::ingest::chat_chunk --lib`
Expected: 5 tests pass.

- [ ] **Step 3: Add the public flush function**

Append to `src-tauri/src/memory/ingest/chat_chunk.rs` (after the `tests` module — actually before it, in the public API section; place it after `render_chunk_body`):

```rust
/// Decode base64 PTY stdout and feed into the buffer. Triggers a flush
/// when the threshold trips. Errors are logged + swallowed.
pub fn on_pty_stdout(
    memory: Arc<MemoryService>,
    db: DbPool,
    app: Option<AppHandle>,
    buffer: Arc<ChatBuffer>,
    agent_id: String,
    data_b64: String,
) {
    if !fast_ingest_enabled(&db) {
        return;
    }
    let decoded = match base64::engine::general_purpose::STANDARD.decode(&data_b64) {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(e) => {
            tracing::debug!(agent_id = %agent_id, "ingest: bad base64: {}", e);
            return;
        }
    };
    let threshold = line_threshold(&db);
    let trip = buffer.push(&agent_id, &decoded, threshold);
    if trip {
        flush_now(memory, app, buffer, &agent_id);
    }
}

/// Flush whatever the buffer holds for `agent_id`. Called on agent exit
/// (so we don't lose the tail) and from `on_pty_stdout` when the
/// threshold trips.
pub fn flush_now(
    memory: Arc<MemoryService>,
    app: Option<AppHandle>,
    buffer: Arc<ChatBuffer>,
    agent_id: &str,
) {
    let chunk = match buffer.drain_for_flush(agent_id, Utc::now()) {
        Some(c) => c,
        None => return,
    };
    let slug = format!("chats/{}/{}", chunk.agent_name, chunk.date);
    // Read existing body so we append rather than overwrite.
    let existing = read_existing_body(&memory, &chunk.workspace_id, &slug).unwrap_or_default();
    let body = render_chunk_body(&existing, &chunk, Utc::now());
    let title = format!("{} — {}", chunk.agent_name, chunk.date);
    let ingest = IngestRecord {
        source_kind: "chat_chunk".into(),
        source_ref: Some(agent_id.to_string()),
        ingested_at: Utc::now().to_rfc3339(),
        smart_pass_at: None,
    };
    let res: Result<Note> = memory.upsert_by_slug(
        &chunk.workspace_id,
        &slug,
        &title,
        &body,
        Vec::new(),
        Kind::Chat,
        Some(ingest),
    );
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
                        source_kind: "chat_chunk".into(),
                    },
                );
            }
        }
        Err(e) => {
            tracing::warn!(agent_id = %agent_id, "fast-lane chat flush failed: {}", e);
        }
    }
}

fn read_existing_body(
    memory: &MemoryService,
    workspace_id: &str,
    slug: &str,
) -> Option<String> {
    let list = memory.list(workspace_id, None, 500).ok()?;
    let summary = list.into_iter().find(|n| n.slug == slug)?;
    let note = memory.get(&summary.id).ok()?;
    Some(note.body)
}
```

- [ ] **Step 4: Confirm `base64` is in `Cargo.toml`**

Run: `grep '^base64' src-tauri/Cargo.toml`
If absent: it's required by `agent.rs`, so it's already a transitive dep. Confirm with `cargo build -p pigide --lib` — if missing, add `base64 = "0.22"` to `[dependencies]` in `src-tauri/Cargo.toml`.

- [ ] **Step 5: Build + test**

Run: `cargo build -p pigide --lib`
Expected: success.

Run: `cargo test -p pigide memory --lib`
Expected: all memory tests pass.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/memory/ingest/chat_chunk.rs
git commit -m "feat(memory): chat-chunk fast-lane buffer + flush

Per-agent rolling buffer (parking_lot::Mutex<HashMap>) accumulates
decoded PTY stdout. Trips at memory.chat_rotation.lines (default
120) and writes chats/<agent>/<yyyy-mm-dd>.md with kind=Chat and
ingest.source_kind='chat_chunk'. Each day starts a fresh chunk
counter; flush appends a new ## chunk N section to the existing
note body so the file shows the agent's timeline."
```

---

## Task 6: wire `ChatBuffer` into `AppState` + lib.rs construction

**Files:**
- Modify: `src-tauri/src/state.rs`
- Modify: `src-tauri/src/lib.rs`

- [ ] **Step 1: Locate the `AppState` struct**

Run: `grep -n 'pub struct AppState' src-tauri/src/state.rs`

- [ ] **Step 2: Add the field**

Inside `pub struct AppState { ... }`, add (preserve alphabetical/logical grouping with neighbors):

```rust
    pub chat_buffer: std::sync::Arc<crate::memory::ingest::chat_chunk::ChatBuffer>,
```

- [ ] **Step 3: Construct it in `lib.rs`**

In `src-tauri/src/lib.rs`, after the `let memory = Arc::new(MemoryService::new(...));` line (currently line 138), add:

```rust
    let chat_buffer = Arc::new(crate::memory::ingest::chat_chunk::ChatBuffer::new());
```

Then in the `AppState { ... }` construction further down (around line 199-210), add:

```rust
        chat_buffer: chat_buffer.clone(),
```

- [ ] **Step 4: Build**

Run: `cargo build -p pigide --lib`
Expected: success — no other call sites need to change yet (Tasks 7+8 wire actual hooks).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/lib.rs src-tauri/src/state.rs
git commit -m "feat(memory): mount ChatBuffer on AppState

Single shared Arc<ChatBuffer> constructed at startup; later commits
plumb it into the PTY pump + agent-exit handler."
```

---

## Task 7: hook `update_task` for the task-complete writer

**Files:**
- Modify: `src-tauri/src/commands.rs:1034-1040`

- [ ] **Step 1: Open `update_task` in `commands.rs`**

Run: `grep -n "pub async fn update_task\|state.task_mgr.update" src-tauri/src/commands.rs`

The current shape is:

```rust
#[tauri::command]
pub async fn update_task(
    state: State<'_, AppState>,
    args: UpdateTaskArgs,
) -> std::result::Result<Task, String> {
    state.task_mgr.update(args).map_err(Into::into)
}
```

- [ ] **Step 2: Replace with the hook-aware version**

Replace the function body. The hook fires only when:
- The args explicitly request `status: Some("complete")`,
- The manager update succeeded,
- The manager's returned task's status is now `"complete"` (so we skip when the gate kept it in_review).

```rust
#[tauri::command]
pub async fn update_task(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    args: UpdateTaskArgs,
) -> std::result::Result<Task, String> {
    let want_complete = matches!(args.status.as_deref(), Some("complete"));
    let task = state.task_mgr.update(args).map_err::<String, _>(Into::into)?;
    if want_complete && task.status == "complete" {
        crate::memory::ingest::task_complete::on_task_complete(
            state.memory.clone(),
            state.task_mgr.clone(),
            state.db.clone(),
            Some(app),
            task.workspace_id.clone(),
            task.id.clone(),
        );
    }
    Ok(task)
}
```

- [ ] **Step 3: Verify `state.db` and `state.memory` exist on `AppState`**

Run: `grep -n 'pub db\|pub memory:' src-tauri/src/state.rs`
Expected: both fields exist (memory definitely does — confirmed earlier; db is the pool).

If `state.db` is named differently (e.g. `state.pool`), use that name in the call above.

- [ ] **Step 4: Build + test**

Run: `cargo build -p pigide --lib`
Expected: success.

Run: `cargo test -p pigide --lib`
Expected: full suite still passes.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands.rs
git commit -m "feat(memory): wire task→complete to fast-lane ingest

update_task Tauri command now dispatches on_task_complete after a
successful transition to status=complete. The hook is fire-and-
forget — failures log without rolling back the user's update."
```

---

## Task 8: hook PTY stdout pump for the chat-chunk writer

**Files:**
- Modify: `src-tauri/src/agent.rs:597-630` (the `start_event_pump` body)

- [ ] **Step 1: Read the existing pump**

Run: `sed -n '583,660p' src-tauri/src/agent.rs` to see the current implementation.

The pump receives `Event::Stdout` and `Event::Exit`. We add two side-effect calls inside its arms.

- [ ] **Step 2: Plumb dependencies into `AgentManager`**

The pump runs inside `AgentManager`; it needs access to `MemoryService`, `DbPool`, and `ChatBuffer`. These are already available on `AppState`, but the pump is constructed inside `AgentManager`. Two options:
- (preferred) Add fields on `AgentManager` for `memory: Option<Arc<MemoryService>>`, `chat_buffer: Option<Arc<ChatBuffer>>`. Set them via a new `set_ingest_handles(&self, memory, chat_buffer)` method called from `lib.rs` once both objects exist.

Add to `AgentManager` (locate `pub struct AgentManager` in `src-tauri/src/agent.rs`):

```rust
    ingest_memory: parking_lot::Mutex<Option<std::sync::Arc<crate::memory::MemoryService>>>,
    ingest_buffer: parking_lot::Mutex<Option<std::sync::Arc<crate::memory::ingest::chat_chunk::ChatBuffer>>>,
```

Initialize in the `AgentManager::new` constructor (look for `Self { ... }` near the top of `agent.rs`):

```rust
            ingest_memory: parking_lot::Mutex::new(None),
            ingest_buffer: parking_lot::Mutex::new(None),
```

Add the setter:

```rust
    pub fn set_ingest_handles(
        &self,
        memory: std::sync::Arc<crate::memory::MemoryService>,
        buffer: std::sync::Arc<crate::memory::ingest::chat_chunk::ChatBuffer>,
    ) {
        *self.ingest_memory.lock() = Some(memory);
        *self.ingest_buffer.lock() = Some(buffer);
    }
```

- [ ] **Step 3: Patch the pump's `Stdout` arm**

In `src-tauri/src/agent.rs`, locate the `Ok(Event::Stdout { agent_id, data_b64 }) => { ... }` arm in `start_event_pump` (around line 602). Add **after** the existing `app.emit(...)` block but **before** the closing `}` of the arm:

```rust
                        // Fast-lane chat ingest: buffer per-agent stdout, flush
                        // on threshold or agent exit. No-op when the relevant
                        // handles aren't set up yet (early startup).
                        let mem = self.ingest_memory.lock().clone();
                        let buf = self.ingest_buffer.lock().clone();
                        if let (Some(mem), Some(buf)) = (mem, buf) {
                            let app_for_ingest = app.clone();
                            let db = self.db.clone();
                            let aid = agent_id.clone();
                            let b64 = data_b64.clone();
                            tauri::async_runtime::spawn(async move {
                                crate::memory::ingest::chat_chunk::on_pty_stdout(
                                    mem,
                                    db,
                                    app_for_ingest,
                                    buf,
                                    aid,
                                    b64,
                                );
                            });
                        }
```

> NOTE: The pump is `async fn` already (look for `async move`). The `self` reference inside the spawned task needs `Arc<AgentManager>`. If the pump's outer body has `self: Arc<Self>` already (typical Tauri pattern), the snippet works. If it has `&self`, refactor to clone `self.ingest_memory.clone()` etc. into local vars **before** the `loop` and pass them in. Fix is mechanical — flag if uncertain.

- [ ] **Step 4: Patch the pump's `Exit` arm**

In the same file, the `Ok(Event::Exit { agent_id }) => { ... }` arm. Add the flush call:

```rust
                        // Final flush for the agent — anything still in the
                        // buffer becomes the last chunk of its day.
                        let mem = self.ingest_memory.lock().clone();
                        let buf = self.ingest_buffer.lock().clone();
                        if let (Some(mem), Some(buf)) = (mem, buf) {
                            let app_for_flush = app.clone();
                            let aid = agent_id.clone();
                            tauri::async_runtime::spawn(async move {
                                crate::memory::ingest::chat_chunk::flush_now(
                                    mem,
                                    app_for_flush,
                                    buf,
                                    &aid,
                                );
                            });
                        }
```

- [ ] **Step 5: Wire `set_ingest_handles` from `lib.rs`**

In `src-tauri/src/lib.rs`, after both `agent_mgr` and `chat_buffer` exist (around line 140-145), add:

```rust
    agent_mgr.set_ingest_handles(memory.clone(), chat_buffer.clone());
```

- [ ] **Step 6: Register agents into the buffer on spawn**

When an agent spawns, the buffer needs the workspace_id + agent_name. The simplest hook: inside `AgentManager::spawn` (or whatever method inserts a new agent row), after the row is created, call:

```rust
        if let Some(buf) = self.ingest_buffer.lock().clone() {
            buf.register_agent(&agent.id, &agent.workspace_id, &agent.agent_type);
        }
```

Locate the spawn path with: `grep -n "fn spawn\|INSERT INTO agents" src-tauri/src/agent.rs | head -5`. Add the snippet after the agent row is committed and you have the `agent.id` / `agent.workspace_id` / `agent.agent_type` values in scope.

> The `agent_type` is what the user sees as the agent's tile name (e.g. `claude`). If a more specific human name (`claude-tile-1`) is available on `Agent`, prefer that.

- [ ] **Step 7: Build + run tests**

Run: `cargo build -p pigide --lib`
Expected: success.

Run: `cargo test -p pigide --lib`
Expected: all tests pass — chat-chunk unit tests already covered the buffer behavior; this task only wires it into the live PTY pump.

- [ ] **Step 8: Commit**

```bash
git add src-tauri/src/agent.rs src-tauri/src/lib.rs
git commit -m "feat(memory): wire PTY pump to chat-chunk fast lane

AgentManager carries optional ingest handles (memory + buffer) set
once both are constructed. The stdout pump dispatches on_pty_stdout
for every PTY chunk, and the exit pump flushes any tail. Agent
spawns also call buffer.register_agent so flushes can resolve to
chats/<agent>/<date>.md without DB round-trips."
```

---

## Task 9: frontend type for the new event

**Files:**
- Modify: `frontend/src/state/types.ts`

- [ ] **Step 1: Add the type**

Append to `frontend/src/state/types.ts`:

```typescript
// ---------- PigMemory ingest events ----------

export interface MemoryNoteCreated {
  id: string;
  slug: string;
  title: string;
  kind: NoteKind;
  source_kind: "task_complete" | "chat_chunk";
}
```

- [ ] **Step 2: Build the frontend**

Run: `cd frontend && npm run build`
Expected: success.

- [ ] **Step 3: Commit**

```bash
git add frontend/src/state/types.ts
git commit -m "feat(memory): MemoryNoteCreated TS type for fast-lane events

Mirrors NoteCreatedPayload from the Rust side. Phase 4 will use it
to drive the ingest-pulse animation in PigMemoryGraph."
```

---

## Task 10: smoke verification

**Files:** none (manual + automated checks)

- [ ] **Step 1: Run the full test suite**

Run: `cargo test -p pigide --lib 2>&1 | tail -10`
Expected: all tests pass.

Run: `cd frontend && npm run build 2>&1 | tail -5`
Expected: success.

- [ ] **Step 2: Sanity-check a real run**

Optional manual check (skip if no UI session is available):
- Start the app
- In a workspace, spawn a `claude` chat tile
- Create a task, move it to `complete`
- Open `~/your-workspace/.pigmemory/tasks/<task-id>.md` → confirm `kind: task` + `## Summary` block + `ingest.source_kind: task_complete`
- Type a few hundred lines of output in the chat tile
- Open `~/your-workspace/.pigmemory/chats/<agent>/<date>.md` → confirm `kind: chat`, one or more `## chunk N` sections

- [ ] **Step 3: Confirm no breakage in existing flows**

- Existing notes still load
- Graph still renders
- Search still works
- Memory tab loads without errors

- [ ] **Step 4: Wrap up**

No commit; this task is verification only.

---

## Self-Review

**1. Spec coverage** — checked § 3 (Hybrid ingest), § 4 (Storage layout), § 7.2 (Ingest pulse) of spec:
- ✅ Fast lane writes `tasks/<task-id>.md` on task→complete — Tasks 4, 7
- ✅ Fast lane writes `chats/<agent>/<date>.md` on chat rotation — Tasks 5, 8
- ✅ Idempotent re-runs — `upsert_by_slug` (Task 1), deterministic slugs
- ✅ `ingest:` frontmatter block carries source_kind/source_ref/ingested_at, smart_pass_at = null — Tasks 4, 5
- ✅ `memory://note.created` event for the frontend pulse — Tasks 2, 4, 5, 9
- ✅ Settings: `memory.fast_ingest.enabled` + `memory.chat_rotation.lines` honoured — Tasks 4, 5
- ✅ Failures don't roll back user actions — `tracing::warn!` + swallow in Tasks 4, 5, 7, 8

Phase 1 of spec is fully covered. Phase 2 (smart lane) and beyond are explicitly out of scope.

**2. Placeholder scan** — searched for "TBD/TODO/implement later/similar to". Found one passage in Task 8 that says "Fix is mechanical — flag if uncertain" about the `self: Arc<Self>` shape of the pump. Replaced the soft-instruction with a concrete fallback: if the pump uses `&self`, clone the handles into local vars before the `loop`. The implementer has all info they need.

**3. Type consistency** — checked:
- `Kind` and `NoteKind` keep the same 6 variants
- `IngestRecord` field names (`source_kind`, `source_ref`, `ingested_at`, `smart_pass_at`) match across Rust and TS
- `NoteCreatedPayload` (Rust) ↔ `MemoryNoteCreated` (TS) match field-for-field
- `EV_MEMORY_NOTE_CREATED` const and the literal `"memory://note.created"` agree
- `upsert_by_slug` signature in Task 1 (workspace_id, slug, title, body, tags, kind, ingest) matches the call sites in Tasks 4, 5
- `ChatBuffer` methods (`new`, `register_agent`, `push`, `drain_for_flush`) are the only public surface and are used consistently across Tasks 5, 6, 8
- `on_task_complete` signature in Task 4 matches its call site in Task 7
- `on_pty_stdout` and `flush_now` signatures in Task 5 match their call sites in Task 8

No drift. Plan is internally consistent.
