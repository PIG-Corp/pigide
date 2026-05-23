# Persistence & Mailbox Audit

**Auditor:** Backend Auditor (task 18f88517)
**Date:** 2026-05-20
**Scope:** SQLite schema, FTS5, transactions, memory store, tasks, review_gates, file_ownership, mailbox/broadcast/rollcall

---

## Files Examined

| File | Purpose |
|------|---------|
| `src-tauri/src/db.rs` | Pool init, migrations v1–v13, WAL, FK pragma |
| `src-tauri/src/memory/service.rs` | MemoryService: CRUD, search (FTS5), backlinks, suggest_connections |
| `src-tauri/src/memory/storage.rs` | Resolve .pigmemory root, slug helpers |
| `src-tauri/src/memory/tools.rs` | MCP tool dispatch for memory subsystem |
| `src-tauri/src/tasks.rs` | TaskManager: CRUD, status transitions, review-gate enforcement |
| `src-tauri/src/swarm/mailbox.rs` | Inter-agent mailbox: send, broadcast, list, mark_read, threads |
| `src-tauri/src/swarm/ownership.rs` | File ownership: acquire, release, release_all_for_task |
| `src-tauri/src/swarm/review.rs` | Review gates: open, vote, task_completable |
| `src-tauri/src/swarm/rollcall.rs` | Rollcall: start (broadcast), respond, collect |

---

## Schema Overview

- **13 migrations** (PRAGMA user_version = 13), idempotent, run on dedicated connection before pool
- WAL mode, busy_timeout=5000, foreign_keys=ON (per-connection via `with_init`)
- Pool: r2d2 max_size=8
- FTS5 tables: `memory_fts` (content-sync with `memory_notes`), `voice_transcripts_fts`
- Triggers: AFTER INSERT/DELETE/UPDATE on content tables keep FTS in sync

---

## Findings

### F1 — [CRITICAL] FTS5 `snippet()` fails: column name mismatch (RCA for "SQL logic error")

**Path:** `src-tauri/src/memory/service.rs:343-365`
**Reproduction:** `search_memories {"query":"orchestrator backend architecture"}` → `{"error":"db: SQL logic error"}`

**Root Cause:**

FTS5 virtual table is defined as:
```sql
CREATE VIRTUAL TABLE memory_fts USING fts5(
    title, body, tags, aliases,
    content='memory_notes', content_rowid='rowid',
    tokenize='unicode61 remove_diacritics 2'
);
```

The FTS5 columns are named `tags` and `aliases`. With `content='memory_notes'`, when `snippet()` is called, FTS5 attempts to read from the content table using the FTS column names — it looks for `memory_notes.tags` and `memory_notes.aliases`.

But `memory_notes` has columns `tags_json` and `aliases_json` — **not** `tags` and `aliases`.

`bm25()` works (uses only the inverted index). `MATCH` works. But `snippet()` needs to read the actual text from the content table and fails with "no such column: T.tags".

**Verified on live DB:**
```
sqlite3> SELECT snippet(memory_fts, 1, ...) FROM memory_fts WHERE memory_fts MATCH '...' → SQL logic error
sqlite3> SELECT bm25(memory_fts, ...) FROM memory_fts WHERE memory_fts MATCH '...' → works
sqlite3> SELECT count(*) FROM memory_fts WHERE memory_fts MATCH '...' → 3
```

**Fix (two options):**

Option A — Rename FTS columns to match content table (requires rebuild):
```sql
DROP TABLE IF EXISTS memory_fts;
CREATE VIRTUAL TABLE memory_fts USING fts5(
    title, body, tags_json, aliases_json,
    content='memory_notes', content_rowid='rowid',
    tokenize='unicode61 remove_diacritics 2'
);
-- Rebuild triggers to use new column names
-- Repopulate: INSERT INTO memory_fts(memory_fts) VALUES('rebuild');
```

Option B (recommended) — Stop using `snippet()`, use `highlight()` on the FTS table directly, or manually extract snippet from `n.body` in Rust:
```diff
--- a/src-tauri/src/memory/service.rs
+++ b/src-tauri/src/memory/service.rs
@@ -343,8 +343,7 @@ impl MemoryService {
         let conn = self.db.get()?;
         let mut stmt = conn.prepare(
             "SELECT n.id, n.slug, n.title,
-                    snippet(memory_fts, 1, '<<', '>>', '…', 16),
+                    substr(n.body, 1, 200),
                     bm25(memory_fts, 4.0, 1.0, 2.0, 1.5)
              FROM memory_fts f
              JOIN memory_notes n ON n.rowid = f.rowid
```

**Best fix:** Migration v14 that renames FTS columns to match content table + rebuilds index. This preserves snippet() functionality.

---

### F2 — [HIGH] `sanitize_fts_query` passes through hyphen as FTS5 NOT operator

**Path:** `src-tauri/src/memory/service.rs:566-587`

The sanitizer allows `-` through:
```rust
if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' { c }
```

Tokens like `-orchestrator` or `--backend` pass the `len() >= 2` filter and become FTS5 NOT expressions. A standalone NOT without a preceding positive term is a syntax error:
```
MATCH '-test' → "no such column: test" (FTS5 interprets as NOT column:test)
MATCH '-test OR hello' → same error
```

**Verified on live DB:**
```
sqlite3> SELECT count(*) FROM memory_fts WHERE memory_fts MATCH '-test'; → Error: no such column: test
```

**Fix:**
```diff
--- a/src-tauri/src/memory/service.rs
+++ b/src-tauri/src/memory/service.rs
@@ -566,7 +566,7 @@ fn sanitize_fts_query(q: &str) -> String {
     let cleaned: String = q
         .chars()
         .map(|c| {
-            if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' {
+            if c.is_alphanumeric() || c == ' ' || c == '_' {
                 c
             } else {
                 ' '
@@ -577,6 +577,7 @@ fn sanitize_fts_query(q: &str) -> String {
     let toks: Vec<String> = cleaned
         .split_whitespace()
         .filter(|s| s.len() >= 2)
+        .filter(|s| !s.starts_with('-'))
         .map(|s| s.to_lowercase())
         .collect();
```

---

### F3 — [MEDIUM] File ownership not released on task status → cancelled

**Path:** `src-tauri/src/tasks.rs:199-265`

`TaskManager::delete()` (line 274) calls `release_all_for_task()`. But `TaskManager::update()` with `status=cancelled` does NOT release file locks. A cancelled task's file locks persist until the task row is deleted (which may never happen — cancelled tasks are kept for history).

The DB schema has `ON DELETE CASCADE` on `file_ownership.task_id`, so if the task row is eventually deleted, locks are cleaned up. But between cancellation and deletion, other tasks cannot claim those files.

**Fix:**
```diff
--- a/src-tauri/src/tasks.rs
+++ b/src-tauri/src/tasks.rs
@@ -229,6 +229,10 @@ impl TaskManager {
                 s
             }
             None => cur.status.clone(),
         };
+        // Release file locks when task is terminal (cancelled or complete).
+        if (status == "cancelled" || status == "complete") && cur.status != status {
+            let _ = crate::swarm::ownership::release_all_for_task(&self.db, &args.id);
+        }
         let agent_id = match args.agent_id {
```

---

### F4 — [MEDIUM] Mailbox role-broadcast not visible to individual agent reads

**Path:** `src-tauri/src/swarm/mailbox.rs:64-103`

`broadcast()` stores mail with `to_addr = "role:builder"`. But `list()` filters by exact `to_addr` match. An agent reading its mailbox by UUID (`list(to="agent-uuid-123")`) will never see role-broadcast messages. The MCP `read_mailbox` tool passes `to` as the agent_id or `role:X` — so agents must know to query BOTH their UUID AND their role address.

This is a design choice (not a bug per se), but it means:
- The MCP tool caller must issue two queries (one for agent_id, one for `role:<role>`)
- Or the `list()` function should accept an agent_id and resolve its role internally

**Recommendation:** Add an `OR` clause in `list()` that also matches `role:<agent's role>` when the caller provides an agent_id.

---

### F5 — [LOW] `suggest_connections` can produce invalid FTS5 query from note body

**Path:** `src-tauri/src/memory/service.rs:403-454`

`suggest_connections()` takes the note's title + first 2000 chars of body and passes through `sanitize_fts_query()`. If the body contains many special chars or is mostly non-alphanumeric (e.g., a code snippet note), all tokens may be filtered out, resulting in the fallback query `"x"` — which searches for the literal letter "x" and returns irrelevant results.

Not a crash, but a quality issue.

---

## Reproductions

| # | Command | Expected | Actual |
|---|---------|----------|--------|
| 1 | `search_memories {"query":"orchestrator backend architecture"}` | Ranked results | `{"error":"db: SQL logic error"}` |
| 2 | `search_memories {"query":"-test"}` | Empty or filtered | `{"error":"db: SQL logic error"}` (via FTS5 NOT) |
| 3 | Cancel task with file locks → try to claim same file from another task | Lock released | Lock persists until DELETE |

---

## SQL Bug RCA Summary

| Field | Value |
|-------|-------|
| **Symptom** | `search_memories` → "db: SQL logic error" |
| **Failing SQL** | `SELECT ... snippet(memory_fts, 1, '<<', '>>', '…', 16) ... FROM memory_fts f JOIN memory_notes n ON n.rowid = f.rowid WHERE n.workspace_root=?1 AND memory_fts MATCH ?2` |
| **Root cause** | FTS5 `content='memory_notes'` maps FTS column names to content table columns by name. FTS has `tags, aliases`; content table has `tags_json, aliases_json`. `snippet()` reads from content table → "no such column: T.tags" |
| **Scope** | Every `search_memories` and `suggest_connections` call |
| **Fix** | Migration v14: DROP + recreate `memory_fts` with columns `title, body, tags_json, aliases_json`; update triggers; `INSERT INTO memory_fts(memory_fts) VALUES('rebuild')` |

---

## Recommendations

1. **Immediate:** Ship migration v14 to fix FTS5 column names (F1). This unblocks all memory search.
2. **Immediate:** Strip hyphens in `sanitize_fts_query` (F2). One-line fix, prevents crash on user input with dashes.
3. **Soon:** Release file locks on task cancellation (F3). Prevents deadlock in multi-task workflows.
4. **Design:** Consider mailbox role-resolution on read (F4). Current design requires callers to know addressing scheme.
5. **Hardening:** Wrap `sanitize_fts_query` fallback `"x"` with a proper empty-result return instead of searching for literal "x" (F5).
