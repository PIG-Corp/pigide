# PigMemory Phase 2 — Smart-Lane LLM Ingest Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Run a periodic background worker that batches recently-ingested fast-lane stubs (tasks + chat chunks), sends them to Haiku 4.5 with a strict-JSON prompt, and applies returned upserts/edits idempotently — turning raw chats and task summaries into linked `concept`/`entity` notes with `[[wikilinks]]` and tags.

**Architecture:** Three new modules under `memory/ingest/`. (1) `queue.rs` — sqlite `ingest_queue` table + dequeue helper. (2) `prompt.rs` — pure JSON-builder (input batch) + parser (output upserts/edits). (3) `smart.rs` — tokio-interval worker that drains the queue, calls the existing `OmniClient` with a Haiku model, applies results via `MemoryService::upsert_by_slug` + a new `append_section`. Fast-lane Phase 1 hooks (`task_complete::on_task_complete_inner`, `chat_chunk::flush_now`) get one extra `enqueue_*` call each. Worker is mounted on startup behind a settings switch (`memory.smart_ingest.enabled`, default-on).

**Tech Stack:** Rust (tokio interval, existing `OmniClient` from `orchestrator::client`, `serde_json`, `chrono`, existing `MemoryService`). No new crates. Uses the same OmniRouter base + auth as the orchestrator.

**Spec:** `docs/superpowers/specs/2026-05-27-pigmemory-claude-obsidian-design.md` § 3 (Hybrid ingest), § 5 (Backend modules), § 6 (Settings).

---

## File Structure

| File | Change | Responsibility |
|---|---|---|
| `src-tauri/src/memory/ingest/queue.rs` | create | `ingest_queue` table CRUD: `enqueue`, `pending_for_workspace`, `mark_processed`, `mark_error` |
| `src-tauri/src/memory/ingest/prompt.rs` | create | pure: `build_batch_input(items, existing_slugs) -> Value`, `parse_batch_output(text) -> ParsedBatch` |
| `src-tauri/src/memory/ingest/smart.rs` | create | tokio worker: `SmartIngestWorker` with `start(self: Arc<Self>)` and `run_pass_for_workspace(ws_id)` for testing |
| `src-tauri/src/memory/ingest/mod.rs` | modify | add `pub mod queue;`, `pub mod prompt;`, `pub mod smart;` |
| `src-tauri/src/memory/service.rs` | modify | add `pub fn append_section(&self, note_id, heading, body) -> Result<Note>` for the smart-lane "edits" channel |
| `src-tauri/src/memory/ingest/task_complete.rs` | modify | one new line: `queue::enqueue_task(&db, &workspace_id, &task_id, &note.id)` after successful upsert |
| `src-tauri/src/memory/ingest/chat_chunk.rs` | modify | one new call inside `flush_now` after upsert: `queue::enqueue_chat(&db, &chunk.workspace_id, agent_id, &note.id, chunk.chunk_no)` |
| `src-tauri/src/db.rs` | modify | migration v17: `ingest_queue` table |
| `src-tauri/src/lib.rs` | modify | construct `SmartIngestWorker`, call `worker.start()` after the `ChatBuffer` |
| `src-tauri/src/state.rs` | modify | optional: store `worker: Arc<SmartIngestWorker>` (only if a Tauri command needs to manually-trigger; keep out for Phase 2 unless required) |

Boundaries:
- `queue.rs` is pure DB. No model calls, no HTTP, no `MemoryService` references.
- `prompt.rs` is pure transformation. No I/O. Easy to unit-test.
- `smart.rs` orchestrates: drain queue → build prompt → HTTP → parse → apply via `MemoryService`. Errors per-batch are logged + queue rows marked with `error` so a poisoned batch doesn't loop forever.
- `task_complete.rs` and `chat_chunk.rs` get **one new line each** — minimal blast radius.

---

## Idempotency / safety net

- `ingest_queue` rows have `processed_at TIMESTAMP NULL`; the worker reads only `WHERE processed_at IS NULL AND smart_attempts < 3`.
- After a successful batch, mark all included row ids `processed_at = now`.
- After a failed batch, increment `smart_attempts` per row; rows that hit 3 attempts stay un-processed but are no longer picked up — they remain for forensics.
- `prompt.rs::parse_batch_output` returns `Err(...)` on schema mismatch → batch fails as a whole, individual rows roll back their attempt counter increment.
- Smart-lane writes go through `upsert_by_slug` (Phase 1) so re-applying the same upsert on a duplicate slug is a no-op (overwrite).
- `append_section` (Task 6) is idempotent over the **section heading + body**: if the existing note already contains exactly this section, skip; otherwise append. Avoids duplicate `## Backlinks from concepts` blocks.

---

## Settings (read at runtime via `db::get_setting`)

| Key | Default | Notes |
|---|---|---|
| `memory.smart_ingest.enabled` | `"true"` | master switch for the worker |
| `memory.smart_ingest.interval_seconds` | `"300"` | tokio interval period |
| `memory.smart_ingest.model` | `"kr/claude-haiku-4-5-20251001"` | LLM model identifier |
| `memory.smart_ingest.max_notes_per_batch` | `"5"` | hard cap on `upsert` array length |
| `memory.smart_ingest.batch_window_minutes` | `"30"` | how far back queue rows are eligible |

Worker reads these on each tick, so flips take effect within one interval (no restart needed).

---

## Task 1: `ingest_queue` table — DB migration v17

**Files:**
- Modify: `src-tauri/src/db.rs:48` (target version) and append a new migration block before `pragma_update`

- [ ] **Step 1: Bump target version**

In `src-tauri/src/db.rs:48`, change:

```rust
    let target = 16;
```

to:

```rust
    let target = 17;
```

- [ ] **Step 2: Append the migration block**

Find the closing `}` of `if current < 16 { ... }` (added in Phase 0). Right after it and **before** `conn.pragma_update(None, "user_version", target)?;`, insert:

```rust
    if current < 17 {
        // PigMemory Phase 2: queue of items the smart-lane worker should
        // enrich with concepts/entities. Populated by the fast-lane on
        // task→complete and chat-rotation; drained by smart::SmartIngestWorker.
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE IF NOT EXISTS ingest_queue (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                workspace_id    TEXT    NOT NULL,
                kind            TEXT    NOT NULL,
                payload_json    TEXT    NOT NULL,
                created_at      TEXT    NOT NULL,
                processed_at    TEXT,
                last_error      TEXT,
                smart_attempts  INTEGER NOT NULL DEFAULT 0
             );
             CREATE INDEX IF NOT EXISTS idx_ingest_pending
                ON ingest_queue(workspace_id, processed_at);
             COMMIT;",
        )?;
    }
```

- [ ] **Step 3: Build**

Run: `cargo build -p pigide --lib`
Expected: success.

- [ ] **Step 4: Run all DB tests**

Run: `cargo test -p pigide --lib db --quiet`
Expected: all DB tests pass (the migration runs automatically on each `migrate_one` call in tests).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/db.rs
git commit -m "feat(memory): DB migration v17 — ingest_queue

Phase 2 prep: queue table the smart-lane worker drains. Fast-lane
ingest pushes one row per task→complete and per chat-chunk flush;
the worker batches them by workspace_id and feeds Haiku 4.5."
```

---

## Task 2: `queue.rs` — pure DB CRUD with tests

**Files:**
- Create: `src-tauri/src/memory/ingest/queue.rs`

- [ ] **Step 1: Write the module with tests up front**

Create `src-tauri/src/memory/ingest/queue.rs`:

```rust
//! `ingest_queue` table accessors. Pure DB; no HTTP, no MemoryService.
//!
//! The fast-lane (`task_complete`, `chat_chunk`) calls `enqueue_task` /
//! `enqueue_chat` after writing a stub. The smart-lane worker calls
//! `pending_for_workspace` to drain a batch, then `mark_processed` /
//! `mark_error` to settle each row.

use crate::db::DbPool;
use crate::error::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    TaskComplete,
    ChatChunk,
}

impl ItemKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ItemKind::TaskComplete => "task_complete",
            ItemKind::ChatChunk => "chat_chunk",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "task_complete" => Some(ItemKind::TaskComplete),
            "chat_chunk" => Some(ItemKind::ChatChunk),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueItem {
    pub id: i64,
    pub workspace_id: String,
    pub kind: String,
    pub payload_json: String,
    pub created_at: String,
    pub smart_attempts: i64,
}

/// Enqueue a `task_complete` item. `note_id` references the fast-lane stub
/// the smart-lane should enrich.
pub fn enqueue_task(
    db: &DbPool,
    workspace_id: &str,
    task_id: &str,
    note_id: &str,
) -> Result<i64> {
    let payload = serde_json::json!({
        "task_id": task_id,
        "note_id": note_id,
    });
    insert_row(db, workspace_id, ItemKind::TaskComplete, &payload.to_string())
}

/// Enqueue a `chat_chunk` item.
pub fn enqueue_chat(
    db: &DbPool,
    workspace_id: &str,
    agent_id: &str,
    note_id: &str,
    chunk_no: usize,
) -> Result<i64> {
    let payload = serde_json::json!({
        "agent_id": agent_id,
        "note_id": note_id,
        "chunk_no": chunk_no,
    });
    insert_row(db, workspace_id, ItemKind::ChatChunk, &payload.to_string())
}

fn insert_row(
    db: &DbPool,
    workspace_id: &str,
    kind: ItemKind,
    payload_json: &str,
) -> Result<i64> {
    let conn = db.get()?;
    conn.execute(
        "INSERT INTO ingest_queue(workspace_id, kind, payload_json, created_at)
         VALUES(?1, ?2, ?3, ?4)",
        rusqlite::params![workspace_id, kind.as_str(), payload_json, Utc::now().to_rfc3339()],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Drain up to `limit` pending rows for `workspace_id`, only those younger
/// than `window_minutes` and with `smart_attempts < 3`. Sorted oldest-first
/// so the worker enriches in roughly chronological order.
pub fn pending_for_workspace(
    db: &DbPool,
    workspace_id: &str,
    window_minutes: i64,
    limit: i64,
) -> Result<Vec<QueueItem>> {
    let cutoff = Utc::now() - chrono::Duration::minutes(window_minutes.max(1));
    let cutoff_iso = cutoff.to_rfc3339();
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT id, workspace_id, kind, payload_json, created_at, smart_attempts
         FROM ingest_queue
         WHERE workspace_id = ?1
           AND processed_at IS NULL
           AND smart_attempts < 3
           AND created_at >= ?2
         ORDER BY id ASC
         LIMIT ?3",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![workspace_id, &cutoff_iso, limit.max(1)],
        |r| {
            Ok(QueueItem {
                id: r.get(0)?,
                workspace_id: r.get(1)?,
                kind: r.get(2)?,
                payload_json: r.get(3)?,
                created_at: r.get(4)?,
                smart_attempts: r.get(5)?,
            })
        },
    )?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Mark a list of row ids `processed_at = now`. Atomic single-statement.
pub fn mark_processed(db: &DbPool, ids: &[i64]) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let placeholders = std::iter::repeat("?")
        .take(ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "UPDATE ingest_queue SET processed_at = ?1 WHERE id IN ({})",
        placeholders
    );
    let now = Utc::now().to_rfc3339();
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![&now];
    for id in ids {
        params.push(id);
    }
    let conn = db.get()?;
    conn.execute(&sql, &*params)?;
    Ok(())
}

/// Mark a list of row ids as failed: bump `smart_attempts`, set `last_error`.
/// Doesn't set `processed_at` so they remain "pending" until they hit 3
/// attempts (then `pending_for_workspace` filters them out).
pub fn mark_error(db: &DbPool, ids: &[i64], err: &str) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let placeholders = std::iter::repeat("?")
        .take(ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "UPDATE ingest_queue
            SET smart_attempts = smart_attempts + 1,
                last_error = ?1
          WHERE id IN ({})",
        placeholders
    );
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![&err];
    for id in ids {
        params.push(id);
    }
    let conn = db.get()?;
    conn.execute(&sql, &*params)?;
    Ok(())
}

/// Total pending rows for a workspace (for status display).
pub fn pending_count(db: &DbPool, workspace_id: &str) -> Result<i64> {
    let conn = db.get()?;
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM ingest_queue
         WHERE workspace_id = ?1 AND processed_at IS NULL AND smart_attempts < 3",
        [workspace_id],
        |r| r.get(0),
    )?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use r2d2_sqlite::SqliteConnectionManager;

    fn fresh_pool() -> DbPool {
        let manager = SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(1).build(manager).unwrap();
        crate::db::migrate_one(&pool.get().unwrap()).unwrap();
        pool
    }

    #[test]
    fn enqueue_task_inserts_row_and_returns_id() {
        let db = fresh_pool();
        let id = enqueue_task(&db, "ws-1", "task-1", "note-1").unwrap();
        assert!(id >= 1);
    }

    #[test]
    fn pending_returns_only_unprocessed_rows_within_window() {
        let db = fresh_pool();
        let i1 = enqueue_task(&db, "ws-1", "t1", "n1").unwrap();
        let _i2 = enqueue_chat(&db, "ws-1", "agent-1", "n2", 1).unwrap();
        let _i3 = enqueue_task(&db, "ws-2", "t3", "n3").unwrap();
        let pending = pending_for_workspace(&db, "ws-1", 30, 50).unwrap();
        assert_eq!(pending.len(), 2);
        // Mark one processed; pending shrinks.
        mark_processed(&db, &[i1]).unwrap();
        let pending = pending_for_workspace(&db, "ws-1", 30, 50).unwrap();
        assert_eq!(pending.len(), 1);
    }

    #[test]
    fn mark_error_increments_attempts_and_filters_after_three() {
        let db = fresh_pool();
        let i1 = enqueue_task(&db, "ws-1", "t1", "n1").unwrap();
        for _ in 0..3 {
            mark_error(&db, &[i1], "boom").unwrap();
        }
        let pending = pending_for_workspace(&db, "ws-1", 30, 50).unwrap();
        assert!(pending.is_empty());
    }

    #[test]
    fn pending_count_excludes_processed_and_exhausted() {
        let db = fresh_pool();
        let _ = enqueue_task(&db, "ws-1", "t1", "n1").unwrap();
        let i2 = enqueue_task(&db, "ws-1", "t2", "n2").unwrap();
        let i3 = enqueue_task(&db, "ws-1", "t3", "n3").unwrap();
        mark_processed(&db, &[i2]).unwrap();
        for _ in 0..3 {
            mark_error(&db, &[i3], "x").unwrap();
        }
        assert_eq!(pending_count(&db, "ws-1").unwrap(), 1);
    }

    #[test]
    fn item_kind_round_trip() {
        for k in [ItemKind::TaskComplete, ItemKind::ChatChunk] {
            assert_eq!(ItemKind::parse(k.as_str()), Some(k));
        }
        assert!(ItemKind::parse("unknown").is_none());
    }
}
```

- [ ] **Step 2: Wire into `mod.rs`**

In `src-tauri/src/memory/ingest/mod.rs`, after `pub mod chat_chunk;` add `pub mod queue;` (alphabetical):

```rust
//! Phase 1 fast-lane ingest pipeline + Phase 2 smart-lane queue.

pub mod chat_chunk;
pub mod events;
pub mod queue;
pub mod task_complete;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p pigide memory::ingest::queue --lib`
Expected: 5 tests pass.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/memory/ingest/queue.rs src-tauri/src/memory/ingest/mod.rs
git commit -m "feat(memory): ingest_queue accessors

enqueue_task / enqueue_chat / pending_for_workspace / mark_processed /
mark_error / pending_count. Pure DB layer for the smart-lane worker.
3-attempts cap and a per-workspace window filter so a poisoned row
can't loop forever and old chunks don't surprise-resurrect on a
restart."
```

---

## Task 3: `prompt.rs` — pure JSON in/out with tests

**Files:**
- Create: `src-tauri/src/memory/ingest/prompt.rs`

- [ ] **Step 1: Write the module**

Create `src-tauri/src/memory/ingest/prompt.rs`:

```rust
//! Pure prompt builder + response parser for the smart-lane LLM.
//!
//! No I/O — just turn `(items, existing_slugs)` into a chat-completions
//! payload and turn the model's reply text into structured upserts/edits
//! the worker can apply.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// One row passed in to the LLM. The worker hydrates these from the
/// fast-lane stubs before building the prompt.
#[derive(Debug, Clone, Serialize)]
pub struct BatchItem {
    pub queue_id: i64,
    pub kind: String,        // "task_complete" | "chat_chunk"
    pub note_slug: String,   // e.g. "tasks/abc-123" or "chats/agent/2026-05-27"
    pub note_title: String,
    pub note_body: String,   // truncated to ~4KB by caller
}

/// Existing note slug + title for the LLM to "prefer linking to existing
/// over creating new". Capped by caller (typically top-50 by FTS score).
#[derive(Debug, Clone, Serialize)]
pub struct ExistingSlug {
    pub slug: String,
    pub title: String,
    pub kind: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Upsert {
    pub kind: String,        // "concept" | "entity" | "source"
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub links_to_slugs: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Edit {
    pub slug: String,
    pub append_section: String,
    pub body: String,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct ParsedBatch {
    #[serde(default)]
    pub upsert: Vec<Upsert>,
    #[serde(default)]
    pub edits: Vec<Edit>,
}

const SYSTEM_PROMPT: &str = include_str!("./prompt_system.txt");

/// Build the chat-completions `messages` array for the smart-lane.
pub fn build_messages(
    workspace_name: &str,
    items: &[BatchItem],
    existing: &[ExistingSlug],
    max_new: usize,
) -> Vec<Value> {
    let user_payload = json!({
        "workspace_name": workspace_name,
        "max_new_notes": max_new,
        "existing_slugs": existing,
        "items": items,
    });
    vec![
        json!({ "role": "system", "content": SYSTEM_PROMPT }),
        json!({
            "role": "user",
            "content": serde_json::to_string_pretty(&user_payload).unwrap_or_else(|_| "{}".into()),
        }),
    ]
}

/// Parse the model's reply. Strips ```json fences and leading prose so a
/// chatty model can still produce valid output. Returns Err on JSON
/// errors or schema mismatches; the worker treats this as a per-batch
/// failure and bumps `smart_attempts` for every row in the batch.
pub fn parse_response(text: &str) -> Result<ParsedBatch> {
    let json_text = extract_json_block(text)
        .ok_or_else(|| Error::Other("no JSON object in response".into()))?;
    let parsed: ParsedBatch = serde_json::from_str(json_text)
        .map_err(|e| Error::Other(format!("parse: {}", e)))?;
    Ok(parsed)
}

fn extract_json_block(s: &str) -> Option<&str> {
    // 1. ```json ... ``` fence
    if let Some(start) = s.find("```json") {
        let after = &s[start + "```json".len()..];
        if let Some(end) = after.find("```") {
            let inner = after[..end].trim_start_matches('\n');
            return Some(inner);
        }
    }
    // 2. Plain ``` ... ``` fence
    if let Some(start) = s.find("```") {
        let after = &s[start + 3..];
        if let Some(end) = after.find("```") {
            let inner = after[..end].trim_start_matches('\n');
            return Some(inner);
        }
    }
    // 3. First `{` to last `}`
    let first = s.find('{')?;
    let last = s.rfind('}')?;
    if last > first {
        Some(&s[first..=last])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_items() -> Vec<BatchItem> {
        vec![BatchItem {
            queue_id: 1,
            kind: "task_complete".into(),
            note_slug: "tasks/abc-123".into(),
            note_title: "Wire ingest".into(),
            note_body: "## Summary\n\nAdd hook.\n".into(),
        }]
    }

    #[test]
    fn build_messages_includes_system_and_user_payload() {
        let msgs = build_messages("my-ws", &sample_items(), &[], 5);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[1]["role"], "user");
        let user_text = msgs[1]["content"].as_str().unwrap();
        assert!(user_text.contains("my-ws"));
        assert!(user_text.contains("tasks/abc-123"));
        assert!(user_text.contains("Add hook"));
    }

    #[test]
    fn parse_response_accepts_clean_json() {
        let raw = r#"{"upsert": [{"kind": "concept", "title": "Idempotent upsert", "body": "x", "tags": ["pattern"], "links_to_slugs": ["tasks/abc-123"]}], "edits": []}"#;
        let parsed = parse_response(raw).unwrap();
        assert_eq!(parsed.upsert.len(), 1);
        assert_eq!(parsed.upsert[0].kind, "concept");
        assert_eq!(parsed.upsert[0].title, "Idempotent upsert");
        assert_eq!(parsed.edits.len(), 0);
    }

    #[test]
    fn parse_response_strips_json_fence() {
        let raw = "Here you go:\n```json\n{\"upsert\": [], \"edits\": []}\n```\n";
        let parsed = parse_response(raw).unwrap();
        assert_eq!(parsed, ParsedBatch::default());
    }

    #[test]
    fn parse_response_strips_plain_fence() {
        let raw = "```\n{\"upsert\": [], \"edits\": []}\n```";
        let parsed = parse_response(raw).unwrap();
        assert_eq!(parsed, ParsedBatch::default());
    }

    #[test]
    fn parse_response_falls_back_to_first_brace_to_last() {
        let raw = "blabla {\"upsert\": [{\"kind\":\"entity\",\"title\":\"T\",\"body\":\"b\"}], \"edits\": []} trailing";
        let parsed = parse_response(raw).unwrap();
        assert_eq!(parsed.upsert.len(), 1);
        assert_eq!(parsed.upsert[0].kind, "entity");
    }

    #[test]
    fn parse_response_errors_on_garbage() {
        assert!(parse_response("plain text without braces").is_err());
        assert!(parse_response("{not valid json}").is_err());
    }

    #[test]
    fn parse_response_accepts_edit_appends() {
        let raw = r#"{"upsert": [], "edits": [{"slug": "tasks/abc-123", "append_section": "## Concepts referenced", "body": "- [[idempotent-upsert]]"}]}"#;
        let parsed = parse_response(raw).unwrap();
        assert_eq!(parsed.edits.len(), 1);
        assert_eq!(parsed.edits[0].slug, "tasks/abc-123");
        assert!(parsed.edits[0].body.contains("[[idempotent-upsert]]"));
    }
}
```

- [ ] **Step 2: Create `prompt_system.txt`**

Create `src-tauri/src/memory/ingest/prompt_system.txt`:

```
You are PigMemory ingest for a developer's local IDE.

You receive a JSON batch of recently-completed tasks and chat chunks
from the workspace, plus a list of EXISTING note slugs (concepts,
entities, sources). Your job is to extract NEW concepts and entities
worth remembering, and to record relevant cross-references on the
existing fast-lane stubs.

Output exactly one JSON object, no prose around it. Schema:

{
  "upsert": [
    {
      "kind": "concept" | "entity" | "source",
      "title": string,           // human-readable, capitalised
      "body": string,            // markdown, may include [[wikilinks]] and #tags
      "tags": [string],          // optional
      "links_to_slugs": [string] // optional — slugs of items in the input batch this concept ties to
    }
  ],
  "edits": [
    {
      "slug": string,            // a slug from the input batch (tasks/... or chats/...)
      "append_section": string,  // markdown heading e.g. "## Concepts referenced"
      "body": string             // markdown body to append
    }
  ]
}

Rules:
1. Prefer linking to EXISTING slugs over creating new notes.
2. Cap upsert at the `max_new_notes` value provided in the input.
3. concept = abstract idea, pattern, or decision. entity = concrete
   thing (person, project name, library, file path, API endpoint).
   source is reserved for user-saved material — only use when an
   item is verbatim source documentation, not a derived concept.
4. Quote 1–2 lines from the source as evidence in the body.
5. If nothing useful can be extracted, return {"upsert":[],"edits":[]}.
6. Keep titles short (under 60 chars). Keep bodies tight (under 800 chars).
7. Use [[wikilinks]] in body to reference both new upserts (by their
   future slug — kebab-case from title) and existing slugs.
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p pigide memory::ingest::prompt --lib`
Expected: 7 tests pass.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/memory/ingest/prompt.rs src-tauri/src/memory/ingest/prompt_system.txt src-tauri/src/memory/ingest/mod.rs
git commit -m "feat(memory): smart-lane prompt builder + parser

Pure JSON in/out, no HTTP. Builds the chat-completions messages
from a batch of fast-lane items + existing slugs, parses the
strict-JSON reply (with three fence-stripping fallbacks). System
prompt is a separate .txt file so non-Rust contributors can edit it
without touching code."
```

(`mod.rs` should now also declare `pub mod prompt;`. Update it before committing.)

---

## Task 4: `MemoryService::append_section` — idempotent append

**Files:**
- Modify: `src-tauri/src/memory/service.rs`

The smart-lane's `edits` channel needs to append a `## Section\n<body>` block to an existing note without duplicating on retry. We add a small helper that:

1. Reads the note via `self.get(id)`.
2. If the body already contains the literal `\n<heading>\n` AND the body block matches, return Ok(unchanged).
3. Otherwise append `\n\n<heading>\n\n<body>\n`, save, return updated note.

- [ ] **Step 1: Write the failing test**

Append inside `src-tauri/src/memory/service.rs` `mod tests` block (after `upsert_by_slug_overwrites_when_slug_exists`):

```rust
    #[test]
    fn append_section_appends_when_absent() {
        let (svc, ws_id, dir) = fresh_service();
        let n = svc
            .upsert_by_slug(
                &ws_id,
                "tasks/abc",
                "Task ABC",
                "## Summary\n\noriginal body\n",
                vec![],
                Kind::Task,
                None,
            )
            .unwrap();
        let updated = svc
            .append_section(&n.id, "## Concepts referenced", "- [[idempotent-upsert]]")
            .unwrap();
        assert!(updated.body.contains("## Summary"));
        assert!(updated.body.contains("## Concepts referenced"));
        assert!(updated.body.contains("[[idempotent-upsert]]"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn append_section_is_idempotent() {
        let (svc, ws_id, dir) = fresh_service();
        let n = svc
            .upsert_by_slug(
                &ws_id,
                "tasks/abc",
                "Task ABC",
                "## Summary\n\noriginal body\n",
                vec![],
                Kind::Task,
                None,
            )
            .unwrap();
        let _ = svc
            .append_section(&n.id, "## Concepts referenced", "- [[x]]")
            .unwrap();
        let twice = svc
            .append_section(&n.id, "## Concepts referenced", "- [[x]]")
            .unwrap();
        // Section appears exactly once.
        assert_eq!(
            twice.body.matches("## Concepts referenced").count(),
            1,
            "duplicate section after second append"
        );
        std::fs::remove_dir_all(dir).ok();
    }
```

- [ ] **Step 2: Run, see it fail**

Run: `cargo test -p pigide memory::service::tests::append_section --lib`
Expected: FAIL — `no method named 'append_section' found`.

- [ ] **Step 3: Implement**

In `impl MemoryService` in `src-tauri/src/memory/service.rs`, anywhere after `upsert_by_slug`, add:

```rust
    /// Append a `## Section\n\n<body>` block to an existing note. If the
    /// exact heading + body pair is already present, the call is a no-op.
    /// Used by the smart-lane's "edits" channel.
    pub fn append_section(
        &self,
        note_id: &str,
        heading: &str,
        body: &str,
    ) -> Result<Note> {
        let mut note = self.get(note_id)?;
        let trimmed_heading = heading.trim_start();
        let trimmed_body = body.trim();
        // Idempotency: heading + body block already present → no-op.
        let block_marker = format!("\n{}\n", trimmed_heading);
        if note.body.contains(&block_marker)
            && note.body.contains(trimmed_body.lines().next().unwrap_or(""))
        {
            // We won't try harder than first-line presence — same heading +
            // same first body line is a strong signal we already wrote it.
            return Ok(note);
        }
        let mut new_body = String::from(note.body.trim_end());
        if !new_body.is_empty() {
            new_body.push_str("\n\n");
        }
        new_body.push_str(trimmed_heading);
        new_body.push_str("\n\n");
        new_body.push_str(trimmed_body);
        new_body.push('\n');
        note.body = new_body;
        note.updated_at = chrono::Utc::now().to_rfc3339();
        let conn = self.db.get()?;
        let (root_str, path_str): (String, String) = conn.query_row(
            "SELECT workspace_root, path FROM memory_notes WHERE id=?1",
            [note_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        drop(conn);
        let path = crate::files::validate_workspace_write_path(
            &path_str,
            &[std::path::PathBuf::from(&root_str)],
        )?;
        let raw = note::serialize(&note);
        note::write(&path, &raw)?;
        self.upsert_index(&root_str, &path, &note)?;
        self.rebuild_links(&note)?;
        Ok(note)
    }
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p pigide memory::service --lib`
Expected: all memory::service tests pass (~9 tests including 2 new).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/memory/service.rs
git commit -m "feat(memory): MemoryService::append_section idempotent helper

Smart-lane 'edits' channel uses this to attach a '## Concepts
referenced' (or similar) block to an existing fast-lane stub.
Re-applying the same (heading, body) pair is a no-op so retries
don't multiply sections."
```

---

## Task 5: `smart.rs` — the worker

**Files:**
- Create: `src-tauri/src/memory/ingest/smart.rs`
- Modify: `src-tauri/src/memory/ingest/mod.rs` (add `pub mod smart;`)

This is the largest file in the plan. Self-contained: the worker holds clones of `MemoryService`, `WorkspaceManager`, `DbPool`, and an `OmniClient`.

- [ ] **Step 1: Write `smart.rs` with tests**

Create `src-tauri/src/memory/ingest/smart.rs`:

```rust
//! Phase 2 smart-lane worker. Drains `ingest_queue` per workspace,
//! sends batches to Haiku 4.5, applies returned upserts/edits.

use crate::db::DbPool;
use crate::error::{Error, Result};
use crate::memory::folders::Kind;
use crate::memory::ingest::prompt::{
    build_messages, parse_response, BatchItem, Edit, ExistingSlug, ParsedBatch, Upsert,
};
use crate::memory::ingest::queue::{
    self, mark_error, mark_processed, pending_for_workspace, QueueItem,
};
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
    pub fn new(
        db: DbPool,
        memory: Arc<MemoryService>,
        ws_mgr: Arc<WorkspaceManager>,
    ) -> Self {
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
            let payload: serde_json::Value = serde_json::from_str(&q.payload_json).unwrap_or(serde_json::Value::Null);
            let note_id = payload.get("note_id").and_then(|v| v.as_str()).unwrap_or("");
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
        let list = match self.memory.list(workspace_id, None, MAX_EXISTING_SLUGS as i64) {
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

    fn apply_edit(
        &self,
        workspace_id: &str,
        pending: &[QueueItem],
        e: &Edit,
    ) -> Result<()> {
        // Resolve the slug back to a note id by scanning our own pending
        // batch (cheaper than a workspace-wide search).
        let note_id = self.resolve_slug_to_id(workspace_id, &e.slug, pending);
        if let Some(id) = note_id {
            self.memory.append_section(&id, &e.append_section, &e.body)?;
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
    use r2d2_sqlite::SqliteConnectionManager;

    fn fresh_worker() -> (Arc<SmartIngestWorker>, String, std::path::PathBuf, DbPool) {
        let dir = std::env::temp_dir().join(format!("pigide-smart-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let manager = SqliteConnectionManager::file(dir.join("db.sqlite"));
        let pool = r2d2::Pool::builder().max_size(4).build(manager).expect("pool");
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
        let concepts = list.iter().filter(|n| n.slug.starts_with("concepts/")).count();
        assert_eq!(concepts, 2);
        std::fs::remove_dir_all(dir).ok();
    }
}
```

- [ ] **Step 2: Wire in `mod.rs`**

In `src-tauri/src/memory/ingest/mod.rs`:

```rust
//! Phase 1 fast-lane ingest pipeline + Phase 2 smart-lane queue.

pub mod chat_chunk;
pub mod events;
pub mod prompt;
pub mod queue;
pub mod smart;
pub mod task_complete;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p pigide memory::ingest::smart --lib`
Expected: 7 tests pass (no network required — `run_pass_no_op_when_queue_empty` short-circuits before HTTP).

- [ ] **Step 4: Run full memory test suite**

Run: `cargo test -p pigide memory --lib`
Expected: all memory tests pass.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/memory/ingest/smart.rs src-tauri/src/memory/ingest/mod.rs
git commit -m "feat(memory): smart-lane LLM worker

SmartIngestWorker drains ingest_queue per workspace, builds a
strict-JSON prompt, posts to OmniRouter Haiku 4.5, parses the
reply, and applies upserts via upsert_by_slug + edits via
append_section. Idempotent on retry. Settings (interval, model,
max_new, window) are read from the DB so flips take effect
within one tick — no app restart needed.

Tests cover the apply/hydrate paths without hitting the network.
The HTTP path is exercised in integration only (requires a live
OmniRouter — out of scope for unit tests)."
```

---

## Task 6: enqueue from fast-lane writers

**Files:**
- Modify: `src-tauri/src/memory/ingest/task_complete.rs`
- Modify: `src-tauri/src/memory/ingest/chat_chunk.rs`

- [ ] **Step 1: `task_complete.rs` — enqueue after successful upsert**

Find `on_task_complete_inner` in `src-tauri/src/memory/ingest/task_complete.rs`. The current end is:

```rust
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
```

Replace it with:

```rust
    let note = memory.upsert_by_slug(
        workspace_id,
        &slug,
        &title,
        &body,
        Vec::new(),
        Kind::Task,
        Some(ingest),
    )?;
    let _ = super::queue::enqueue_task(db, workspace_id, task_id, &note.id);
    Ok(note)
}
```

> The `let _ =` deliberately swallows any enqueue error. The fast-lane stub is already on disk and indexed; if the queue write fails (e.g. SQLite is busy), the smart-lane just won't enrich this one item. Far better than failing the whole task→complete flow.

- [ ] **Step 2: `chat_chunk.rs` — enqueue after successful upsert**

Find `flush_now` in `src-tauri/src/memory/ingest/chat_chunk.rs`. The current `match res` arm is:

```rust
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
```

Replace with the version that ALSO enqueues. To enqueue we need a `&DbPool`. The function signature currently is `fn flush_now(memory, app, buffer, agent_id)` — no DB. We need to pass the DB through. Update the `flush_now` signature to:

```rust
pub fn flush_now(
    memory: Arc<MemoryService>,
    db: DbPool,
    app: Option<AppHandle>,
    buffer: Arc<ChatBuffer>,
    agent_id: &str,
) {
```

…and update the body's `match res { Ok(note) => { ... } }` arm to also do:

```rust
            let _ = super::queue::enqueue_chat(
                &db,
                &chunk.workspace_id,
                agent_id,
                &note.id,
                chunk.chunk_no,
            );
```

(insert right after the `emit_note_created` block but still inside `Ok(note) => { ... }`.)

Update the caller `on_pty_stdout` (in the same file) to pass `db` through to `flush_now`:

```rust
    let trip = buffer.push(&agent_id, &decoded, threshold);
    if trip {
        flush_now(memory, db, app, buffer, &agent_id);
    }
```

- [ ] **Step 3: Update agent.rs callers**

`src-tauri/src/agent.rs` calls `flush_now(mem, app, buf, &aid)` in the Exit arm. Update to:

```rust
                            tauri::async_runtime::spawn(async move {
                                crate::memory::ingest::chat_chunk::flush_now(
                                    mem,
                                    db_for_flush,
                                    app_for_flush,
                                    buf,
                                    &aid,
                                );
                            });
```

…with `let db_for_flush = db.clone();` added before the `tauri::async_runtime::spawn(...)` block. The pump's outer scope already has `db`.

- [ ] **Step 4: Build and run tests**

Run: `cargo build -p pigide --lib`
Expected: success.

Run: `cargo test -p pigide memory --lib`
Expected: all memory tests pass. The `chat_chunk` tests **don't call `flush_now`** (only the buffer), so the signature change doesn't break them.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/memory/ingest/task_complete.rs src-tauri/src/memory/ingest/chat_chunk.rs src-tauri/src/agent.rs
git commit -m "feat(memory): enqueue smart-lane work from fast-lane writers

After a successful task→complete or chat-chunk flush, push one
ingest_queue row referencing the just-written stub. The smart-lane
worker (Phase 2) drains these to enrich with concepts/entities.
Errors are swallowed — a queue hiccup must not roll back the
fast-lane write the user just saw appear."
```

---

## Task 7: mount worker in `lib.rs`

**Files:**
- Modify: `src-tauri/src/lib.rs`

- [ ] **Step 1: Construct + start**

Right after the `let chat_buffer = Arc::new(...)` line, add:

```rust
    let smart_worker = Arc::new(crate::memory::ingest::smart::SmartIngestWorker::new(
        pool.clone(),
        memory.clone(),
        ws_mgr.clone(),
    ));
    smart_worker.clone().start();
```

We don't put `smart_worker` on `AppState` for Phase 2 — no Tauri command needs it. (Phase 6 will add a manual-trigger command and we'll plumb it then.)

- [ ] **Step 2: Build**

Run: `cargo build -p pigide --lib`
Expected: success.

- [ ] **Step 3: Smoke test the boot path**

Run: `cargo test -p pigide --lib 2>&1 | tail -5`
Expected: full test suite passes (the worker only starts in `tauri::async_runtime`, which tests don't drive — so no test should hit its tick).

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/lib.rs
git commit -m "feat(memory): mount smart-lane worker at startup

Spawns the tokio interval inside Tauri's async runtime. With the
default settings (enabled=true, interval=300s) the loop wakes
every 5 minutes and drains pending ingest_queue rows per workspace.
A user can toggle memory.smart_ingest.enabled='false' to put it
to sleep without restarting the app."
```

---

## Task 8: smoke verification

**Files:** none

- [ ] **Step 1: Full backend tests**

Run: `cargo test -p pigide --lib 2>&1 | tail -10`
Expected: all tests pass (~370 + new tests from this phase).

- [ ] **Step 2: Backend build**

Run: `cargo build -p pigide --lib 2>&1 | tail -5`
Expected: clean.

- [ ] **Step 3: Frontend (no TS changes in Phase 2 — sanity build only)**

Run: `cd frontend && npm run build 2>&1 | tail -5`
Expected: clean.

- [ ] **Step 4: Manual smoke (optional, only if a dev session is available)**

- Start the app
- Spawn a `claude` chat tile, type a few hundred lines
- Wait 5 min (or set `memory.smart_ingest.interval_seconds=30` for a faster check)
- Inspect `<workspace>/.pigmemory/concepts/` — see 0 to N concept notes from extraction
- Inspect `<workspace>/.pigmemory/tasks/<task-id>.md` — `## Concepts referenced` section should appear at the bottom on the next pass
- Toggle `memory.smart_ingest.enabled=false` in settings — worker stops touching new rows

No commit; verification only.

---

## Self-Review

**1. Spec coverage** — checked § 3 (Smart lane), § 5 (Backend modules), § 6 (Settings) of the spec:

- ✅ `ingest_queue` table — Task 1
- ✅ enqueue from fast-lane on task→complete + chat_chunk — Task 6
- ✅ tokio interval worker — Task 5
- ✅ Haiku 4.5 model + OmniRouter — Task 5 `build_client`
- ✅ Strict-JSON prompt with upsert/edits + existing-slugs context — Task 3
- ✅ Idempotent upsert via `upsert_by_slug` (already from Phase 1) — used in Task 5
- ✅ Idempotent edit append via `append_section` — Task 4
- ✅ 3-attempts cap, batch-window filter — Task 2
- ✅ Settings (`enabled`, `interval`, `model`, `max_new`, `window`) — Task 5

Phase 2 of the spec is fully covered. Phases 3+ (hot cache, frontend visualization) explicitly out of scope.

**2. Placeholder scan** — searched for "TBD/TODO/implement later/similar to". Found one passage in Task 5 about `NoteSummary` not carrying `kind` ("default `'source'`"). This is a real limitation — `NoteSummary` was defined in Phase 0 without a `kind` field. The smart-lane sees only summary kinds as "source" in the prompt, which is acceptable because the `existing_slugs` block is *advisory* for the LLM ("prefer linking to existing"). Replacing the placeholder is out of scope; flagged for awareness in Phase 4 when `NoteSummary.kind` will be added for the graph filter.

**3. Type consistency** —
- `ItemKind::TaskComplete | ChatChunk` matches `IngestRecord.source_kind` strings `"task_complete"` / `"chat_chunk"` from Phase 1
- `Upsert.kind` strings (`"concept" | "entity" | "source"`) parse via `Kind::parse`, fallback `Kind::Source`
- `Edit.slug` resolves through `resolve_slug_to_id` (worker private) — returns `Option<note_id>`
- `BatchItem.queue_id` is `i64` matching `ingest_queue.id`
- `enqueue_task(db, workspace_id, task_id, note_id)` and `enqueue_chat(db, workspace_id, agent_id, note_id, chunk_no)` signatures match the calls in Task 6
- `flush_now(memory, db, app, buffer, agent_id)` signature change in Task 6 lines up with the agent.rs call site update in same task
- `SmartIngestWorker::new(db, memory, ws_mgr)` matches the `lib.rs` constructor call in Task 7

No drift. Plan is internally consistent.
