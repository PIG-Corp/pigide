# PigMemory Phase 0 — Storage Rework Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `.pigmemory/` ready for Phase 1+ ingest — allow nested folders in slugs (`tasks/abc-123`), add `kind` and `ingest` fields to note frontmatter, migrate existing notes idempotently. No behavior change for the user; this is groundwork.

**Architecture:** Three small, surgical edits to `memory/storage.rs` and `memory/note.rs`, one DB migration, one one-shot in-process migration on startup. Existing flat notes are auto-tagged `kind: source` so they stay searchable. New notes default to `kind: source` until later phases set the right kind explicitly.

**Tech Stack:** Rust (rustc 1.79+), rusqlite, chrono, serde. Tests via `cargo test -p pigide` (the workspace name in `Cargo.toml`). No new crate dependencies.

**Spec:** `docs/superpowers/specs/2026-05-27-pigmemory-claude-obsidian-design.md` § 4 (Storage layout).

---

## File Structure

| File | Change | Responsibility |
|---|---|---|
| `src-tauri/src/memory/storage.rs` | modify | accept nested slugs (`a/b/c`); reject traversal/bad chars |
| `src-tauri/src/memory/note.rs` | modify | extend `Note` struct with `kind` + `ingest`; serialize/parse new frontmatter |
| `src-tauri/src/memory/folders.rs` | create | `Kind` enum + `kind_for_slug` helper + folder-prefix mapping |
| `src-tauri/src/memory/mod.rs` | modify | `pub mod folders;` |
| `src-tauri/src/memory/service.rs` | modify | thread `kind` through `create`/`update`/`get`/`graph`; `unique_slug` works with nested |
| `src-tauri/src/memory/tools.rs` | modify | optional `kind` arg in `create_memory`; pass-through in `update_memory` |
| `src-tauri/src/db.rs` | modify | migration v16: add `kind TEXT` + `ingest_json TEXT` columns to `memory_notes` |
| `src-tauri/src/memory/migration.rs` | create | one-shot disk migration: re-serialize old notes with `kind:source` |
| `src-tauri/src/lib.rs` | modify | call `memory::migration::run_once` after DB migrations on startup |
| `frontend/src/state/types.ts` | modify | add `kind?: NoteKind` to `Note`/`NoteSummary`/`GraphNode` |

Boundaries: `folders.rs` owns the kind ↔ folder mapping (nothing else should hardcode `"concepts/"`). `migration.rs` runs once at startup and exits — never called from runtime paths.

---

## Task 1: Allow nested slugs in `storage::validate_slug`

**Files:**
- Modify: `src-tauri/src/memory/storage.rs:46-57`
- Test: `src-tauri/src/memory/storage.rs` (the existing `mod tests` block)

- [ ] **Step 1: Replace the existing `slug_rejects_path_separators_and_dot_segments` test with the new behavior**

Open `src-tauri/src/memory/storage.rs` and replace the body of `slug_rejects_path_separators_and_dot_segments` (lines 97-104) plus add three new tests:

```rust
    #[test]
    fn slug_accepts_single_level_nesting() {
        let root = tempdir_for_test("pigide-memory-nest");
        let p = slug_to_path(&root, "tasks/abc-123").unwrap();
        assert_eq!(p, root.join("tasks").join("abc-123.md"));
        let back = path_to_slug(&root, &p).unwrap();
        assert_eq!(back, "tasks/abc-123");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn slug_accepts_two_level_nesting() {
        let root = tempdir_for_test("pigide-memory-nest2");
        let p = slug_to_path(&root, "chats/claude-tile-1/2026-05-27").unwrap();
        assert_eq!(
            p,
            root.join("chats").join("claude-tile-1").join("2026-05-27.md")
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn slug_rejects_traversal_and_bad_chars() {
        let root = tempdir_for_test("pigide-memory-bad");
        assert!(slug_to_path(&root, "../etc/passwd").is_err());
        assert!(slug_to_path(&root, "tasks/../etc").is_err());
        assert!(slug_to_path(&root, "tasks//double").is_err());
        assert!(slug_to_path(&root, "/abs").is_err());
        assert!(slug_to_path(&root, "trailing/").is_err());
        assert!(slug_to_path(&root, r"with\backslash").is_err());
        assert!(slug_to_path(&root, "with\0null").is_err());
        assert!(slug_to_path(&root, ".").is_err());
        assert!(slug_to_path(&root, "").is_err());
        std::fs::remove_dir_all(root).ok();
    }
```

Then **delete** the old `slug_rejects_path_separators_and_dot_segments` test (lines 97-104) and the old `slug_rejects_traversal` test (lines 89-95) — both are subsumed by `slug_rejects_traversal_and_bad_chars`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p pigide memory::storage::tests --lib`
Expected: FAIL — `slug_accepts_single_level_nesting` and `slug_accepts_two_level_nesting` fail with `Invalid("invalid memory slug: tasks/abc-123")`. The new traversal test may also fail because the current code treats any `/` as bad.

- [ ] **Step 3: Replace `validate_slug` with the nested-aware version**

Replace lines 46-57 of `src-tauri/src/memory/storage.rs` with:

```rust
fn validate_slug(slug: &str) -> Result<()> {
    if slug.is_empty() || slug == "." {
        return Err(Error::Invalid(format!("invalid memory slug: {}", slug)));
    }
    if slug.starts_with('/') || slug.ends_with('/') {
        return Err(Error::Invalid(format!("invalid memory slug: {}", slug)));
    }
    if slug.contains('\\') || slug.contains('\0') || slug.contains("//") {
        return Err(Error::Invalid(format!("invalid memory slug: {}", slug)));
    }
    for segment in slug.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(Error::Invalid(format!("invalid memory slug: {}", slug)));
        }
    }
    Ok(())
}
```

Also update `slug_to_path` (line 32) to create the parent directory when nested. Replace lines 32-44 with:

```rust
pub fn slug_to_path(root: &Path, slug: &str) -> Result<PathBuf> {
    validate_slug(slug)?;
    let base = root.canonicalize()?;
    let mut p = base.join(slug);
    p.set_extension("md");
    if !p.starts_with(&base) {
        return Err(Error::Invalid(format!(
            "slug escapes memory root: {}",
            slug
        )));
    }
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(p)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p pigide memory::storage::tests --lib`
Expected: PASS for `slug_round_trip`, `slug_accepts_single_level_nesting`, `slug_accepts_two_level_nesting`, `slug_rejects_traversal_and_bad_chars`, `slugify_strips_punctuation`.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/memory/storage.rs
git commit -m "feat(memory): allow nested slugs in .pigmemory/

Permits 'tasks/abc-123' and 'chats/agent/2026-05-27' style slugs
while still rejecting traversal, backslash, NUL, leading/trailing
slash, and double slash. Auto-creates the parent directory on write.

Phase 0 of the PigMemory ingest rework (spec 2026-05-27)."
```

---

## Task 2: Add `Kind` enum + folder mapping in `memory::folders`

**Files:**
- Create: `src-tauri/src/memory/folders.rs`
- Modify: `src-tauri/src/memory/mod.rs:8`

- [ ] **Step 1: Create the new module file with tests up front**

Create `src-tauri/src/memory/folders.rs`:

```rust
//! Mapping between PigMemory `Kind` and on-disk folder prefix.
//!
//! Centralises the kind-to-folder convention so nothing else hardcodes
//! `"concepts/"` etc. The default kind for a flat-slug note is `Source`
//! (legacy notes from before Phase 0 land in this bucket).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Concept,
    Entity,
    Source,
    Task,
    Chat,
    Meta,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Concept => "concept",
            Kind::Entity => "entity",
            Kind::Source => "source",
            Kind::Task => "task",
            Kind::Chat => "chat",
            Kind::Meta => "meta",
        }
    }

    pub fn folder(self) -> &'static str {
        match self {
            Kind::Concept => "concepts",
            Kind::Entity => "entities",
            Kind::Source => "sources",
            Kind::Task => "tasks",
            Kind::Chat => "chats",
            Kind::Meta => "meta",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        match s {
            "concept" => Some(Kind::Concept),
            "entity" => Some(Kind::Entity),
            "source" => Some(Kind::Source),
            "task" => Some(Kind::Task),
            "chat" => Some(Kind::Chat),
            "meta" => Some(Kind::Meta),
            _ => None,
        }
    }

    /// Default kind used when a note has no `kind` field on disk yet.
    pub fn default_for_legacy() -> Kind {
        Kind::Source
    }
}

/// Best-effort guess from the slug's leading folder. Used only by the
/// migration to assign a kind to old notes that happen to live in a
/// recognisable folder; otherwise falls back to `Source`.
pub fn kind_for_slug(slug: &str) -> Kind {
    let leading = slug.split('/').next().unwrap_or(slug);
    match leading {
        "concepts" => Kind::Concept,
        "entities" => Kind::Entity,
        "sources" => Kind::Source,
        "tasks" => Kind::Task,
        "chats" => Kind::Chat,
        "meta" => Kind::Meta,
        _ => Kind::default_for_legacy(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_str_round_trip() {
        for k in [
            Kind::Concept,
            Kind::Entity,
            Kind::Source,
            Kind::Task,
            Kind::Chat,
            Kind::Meta,
        ] {
            assert_eq!(Kind::parse(k.as_str()), Some(k));
        }
    }

    #[test]
    fn folder_is_pluralised() {
        assert_eq!(Kind::Concept.folder(), "concepts");
        assert_eq!(Kind::Task.folder(), "tasks");
        assert_eq!(Kind::Meta.folder(), "meta");
    }

    #[test]
    fn legacy_default_is_source() {
        assert_eq!(Kind::default_for_legacy(), Kind::Source);
        assert_eq!(kind_for_slug("auth-pattern"), Kind::Source);
    }

    #[test]
    fn kind_for_slug_recognises_folders() {
        assert_eq!(kind_for_slug("tasks/abc-123"), Kind::Task);
        assert_eq!(kind_for_slug("concepts/idempotent-upsert"), Kind::Concept);
        assert_eq!(kind_for_slug("chats/claude/2026-05-27"), Kind::Chat);
        assert_eq!(kind_for_slug("meta/hot"), Kind::Meta);
    }
}
```

- [ ] **Step 2: Wire the module up in `mod.rs`**

In `src-tauri/src/memory/mod.rs`, after line 7 (`pub mod links;`) insert:

```rust
pub mod folders;
```

So the file becomes:

```rust
pub mod folders;
pub mod links;
pub mod note;
pub mod service;
pub mod storage;
pub mod tools;
pub mod watcher;

pub use service::MemoryService;
```

- [ ] **Step 3: Run the new tests**

Run: `cargo test -p pigide memory::folders::tests --lib`
Expected: PASS — 4 tests (`kind_str_round_trip`, `folder_is_pluralised`, `legacy_default_is_source`, `kind_for_slug_recognises_folders`).

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/memory/folders.rs src-tauri/src/memory/mod.rs
git commit -m "feat(memory): add Kind enum + folder mapping

Centralises kind-to-folder convention (concepts/entities/sources/
tasks/chats/meta) so later phases don't hardcode folder names.
Default kind for legacy flat notes is Source."
```

---

## Task 3: Extend `Note` struct with `kind` + `ingest`

**Files:**
- Modify: `src-tauri/src/memory/note.rs:10-22, 40-55, 58-84, 101-157`
- Test: `src-tauri/src/memory/note.rs:249-275` (add new tests, keep existing)

- [ ] **Step 1: Write the new failing tests at the bottom of the existing `mod tests` block**

In `src-tauri/src/memory/note.rs`, inside the existing `#[cfg(test)] mod tests { ... }` (line 249), append after `raw_markdown_without_frontmatter`:

```rust
    #[test]
    fn round_trip_preserves_kind_and_ingest() {
        let mut n = Note::new(
            "tasks/abc-123".into(),
            "Task ABC".into(),
            "did the thing\n".into(),
        );
        n.kind = crate::memory::folders::Kind::Task;
        n.ingest = Some(IngestRecord {
            source_kind: "task".into(),
            source_ref: Some("abc-123".into()),
            ingested_at: "2026-05-27T15:00:00Z".into(),
            smart_pass_at: None,
        });
        let raw = serialize(&n);
        assert!(raw.contains("kind: task"));
        assert!(raw.contains("ingest:"));
        assert!(raw.contains("source_kind: task"));
        let parsed = parse("tasks/abc-123", &raw).unwrap();
        assert_eq!(parsed.kind, crate::memory::folders::Kind::Task);
        let ing = parsed.ingest.expect("ingest preserved");
        assert_eq!(ing.source_kind, "task");
        assert_eq!(ing.source_ref.as_deref(), Some("abc-123"));
        assert!(ing.smart_pass_at.is_none());
    }

    #[test]
    fn legacy_note_without_kind_defaults_to_source() {
        let raw = "---\nid: 11111111-1111-1111-1111-111111111111\ntitle: Old\ncreated_at: 2025-01-01T00:00:00Z\nupdated_at: 2025-01-01T00:00:00Z\n---\nbody\n";
        let n = parse("old-flat", raw).unwrap();
        assert_eq!(n.kind, crate::memory::folders::Kind::Source);
        assert!(n.ingest.is_none());
    }

    #[test]
    fn smart_pass_at_round_trips_when_set() {
        let mut n = Note::new("concepts/x".into(), "X".into(), "".into());
        n.kind = crate::memory::folders::Kind::Concept;
        n.ingest = Some(IngestRecord {
            source_kind: "task_complete".into(),
            source_ref: Some("t-1".into()),
            ingested_at: "2026-05-27T15:00:00Z".into(),
            smart_pass_at: Some("2026-05-27T15:05:00Z".into()),
        });
        let raw = serialize(&n);
        let parsed = parse("concepts/x", &raw).unwrap();
        assert_eq!(
            parsed.ingest.unwrap().smart_pass_at.as_deref(),
            Some("2026-05-27T15:05:00Z")
        );
    }
```

- [ ] **Step 2: Run tests to verify they fail (compile error is expected)**

Run: `cargo test -p pigide memory::note::tests --lib`
Expected: FAIL with `error[E0609]: no field 'kind' on type 'Note'` and similar for `ingest`. This is the test-first signal.

- [ ] **Step 3: Add `IngestRecord` and extend `Note`**

In `src-tauri/src/memory/note.rs`, replace lines 1-7 (the imports + doc comment) with:

```rust
//! Note model + YAML frontmatter (de)serialization.

use crate::error::{Error, Result};
use crate::memory::folders::Kind;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Where this note came from. `None` for user-created notes; `Some(...)`
/// for anything written by the ingest pipeline.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct IngestRecord {
    pub source_kind: String,
    #[serde(default)]
    pub source_ref: Option<String>,
    pub ingested_at: String,
    #[serde(default)]
    pub smart_pass_at: Option<String>,
}
```

Then replace the `Note` struct (lines 10-22) with:

```rust
/// In-memory representation of a note. The `path` field is workspace-relative
/// (or absolute, depending on caller); the frontmatter holds the canonical id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Note {
    pub id: String,
    pub slug: String,
    pub title: String,
    #[serde(default = "Kind::default_for_legacy")]
    pub kind: Kind,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub body: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub ingest: Option<IngestRecord>,
}
```

Then replace `Note::new` (lines 24-38) with:

```rust
impl Note {
    pub fn new(slug: String, title: String, body: String) -> Self {
        let ts = Utc::now().to_rfc3339();
        Self {
            id: Uuid::new_v4().to_string(),
            slug,
            title,
            kind: Kind::default_for_legacy(),
            tags: Vec::new(),
            aliases: Vec::new(),
            body,
            created_at: ts.clone(),
            updated_at: ts,
            ingest: None,
        }
    }
}
```

- [ ] **Step 4: Update `serialize` to emit `kind` and `ingest`**

Replace the `serialize` function (lines 58-84) with:

```rust
/// Serialize a note to a markdown string with YAML frontmatter.
pub fn serialize(note: &Note) -> String {
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("id: {}\n", note.id));
    out.push_str(&format!("title: {}\n", yaml_escape(&note.title)));
    out.push_str(&format!("kind: {}\n", note.kind.as_str()));
    if !note.tags.is_empty() {
        out.push_str(&format!("tags: [{}]\n", note.tags.join(", ")));
    }
    if !note.aliases.is_empty() {
        out.push_str(&format!(
            "aliases: [{}]\n",
            note.aliases
                .iter()
                .map(|a| yaml_escape(a))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    out.push_str(&format!("created_at: {}\n", note.created_at));
    out.push_str(&format!("updated_at: {}\n", note.updated_at));
    if let Some(ing) = &note.ingest {
        out.push_str("ingest:\n");
        out.push_str(&format!("  source_kind: {}\n", ing.source_kind));
        if let Some(r) = &ing.source_ref {
            out.push_str(&format!("  source_ref: {}\n", yaml_escape(r)));
        }
        out.push_str(&format!("  ingested_at: {}\n", ing.ingested_at));
        if let Some(s) = &ing.smart_pass_at {
            out.push_str(&format!("  smart_pass_at: {}\n", s));
        }
    }
    out.push_str("---\n");
    out.push_str(&note.body);
    if !note.body.ends_with('\n') {
        out.push('\n');
    }
    out
}
```

- [ ] **Step 5: Update `parse_frontmatter` to read `kind` and the `ingest:` block**

Replace `parse_frontmatter` (lines 101-157) with:

```rust
fn parse_frontmatter(slug: &str, fm: &str, body: &str) -> Result<Note> {
    // Hand-rolled tiny YAML reader, extended to recognise a one-level
    // nested `ingest:` block of the form
    //
    //   ingest:
    //     source_kind: task
    //     source_ref: abc-123
    //     ingested_at: 2026-...
    //     smart_pass_at: 2026-...
    let mut id = String::new();
    let mut title = String::new();
    let mut kind = Kind::default_for_legacy();
    let mut tags = Vec::new();
    let mut aliases = Vec::new();
    let mut created_at = String::new();
    let mut updated_at = String::new();
    let mut in_ingest = false;
    let mut ing_source_kind = String::new();
    let mut ing_source_ref: Option<String> = None;
    let mut ing_ingested_at = String::new();
    let mut ing_smart_pass_at: Option<String> = None;
    let mut ingest_seen = false;
    for raw_line in fm.lines() {
        let line = raw_line.trim_end();
        if line.is_empty() {
            continue;
        }
        let starts_indented = line.starts_with("  ") || line.starts_with('\t');
        if in_ingest && starts_indented {
            let trimmed = line.trim_start();
            let (k, v) = match trimmed.split_once(':') {
                Some(x) => x,
                None => continue,
            };
            let val = v.trim();
            match k.trim() {
                "source_kind" => ing_source_kind = yaml_unescape(val),
                "source_ref" => {
                    if !val.is_empty() {
                        ing_source_ref = Some(yaml_unescape(val));
                    }
                }
                "ingested_at" => ing_ingested_at = val.to_string(),
                "smart_pass_at" => {
                    if !val.is_empty() {
                        ing_smart_pass_at = Some(val.to_string());
                    }
                }
                _ => {}
            }
            continue;
        }
        in_ingest = false;
        let (k, v) = match line.split_once(':') {
            Some(x) => x,
            None => continue,
        };
        let key = k.trim();
        let val = v.trim();
        match key {
            "id" => id = val.to_string(),
            "title" => title = yaml_unescape(val),
            "kind" => {
                if let Some(parsed) = Kind::parse(val) {
                    kind = parsed;
                }
            }
            "tags" => tags = parse_inline_list(val),
            "aliases" => {
                aliases = parse_inline_list(val)
                    .into_iter()
                    .map(|s| yaml_unescape(&s))
                    .collect()
            }
            "created_at" => created_at = val.to_string(),
            "updated_at" => updated_at = val.to_string(),
            "ingest" => {
                in_ingest = true;
                ingest_seen = true;
            }
            _ => {}
        }
    }
    let id = if id.is_empty() {
        Uuid::new_v4().to_string()
    } else {
        id
    };
    let now = Utc::now().to_rfc3339();
    let ingest = if ingest_seen && !ing_source_kind.is_empty() {
        Some(IngestRecord {
            source_kind: ing_source_kind,
            source_ref: ing_source_ref,
            ingested_at: if ing_ingested_at.is_empty() {
                now.clone()
            } else {
                ing_ingested_at
            },
            smart_pass_at: ing_smart_pass_at,
        })
    } else {
        None
    };
    Ok(Note {
        id,
        slug: slug.to_string(),
        title,
        kind,
        tags,
        aliases,
        body: body.to_string(),
        created_at: if created_at.is_empty() {
            now.clone()
        } else {
            created_at
        },
        updated_at: if updated_at.is_empty() {
            now
        } else {
            updated_at
        },
        ingest,
    })
}
```

Also update the dead `Frontmatter` helper struct (lines 40-55) — since it's `#[allow(dead_code)]` and serves no runtime purpose, leave its fields list **as-is**. (This struct is never instantiated; touching it now risks unrelated churn.)

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p pigide memory::note::tests --lib`
Expected: PASS for all 5 tests including the 3 new ones.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/memory/note.rs
git commit -m "feat(memory): kind + ingest in note frontmatter

Note now carries kind: concept|entity|source|task|chat|meta and
an optional ingest: { source_kind, source_ref, ingested_at,
smart_pass_at } block. Legacy notes without the field default to
kind: source on parse. Round-trip preserved."
```

---

## Task 4: Thread `kind`/`ingest` through `MemoryService`

**Files:**
- Modify: `src-tauri/src/memory/service.rs:42-47, 80-106, 108-125, 212-272`
- Test: `src-tauri/src/memory/service.rs:634-668` (extend the existing tests block; need to add an integration-style test)

- [ ] **Step 1: Write the failing test for `create + get + graph` carrying `kind`**

Append to the `#[cfg(test)] mod tests` block at the bottom of `src-tauri/src/memory/service.rs`:

```rust
    use crate::memory::folders::Kind;
    use crate::workspace::WorkspaceManager;

    fn fresh_service() -> (MemoryService, String, std::path::PathBuf) {
        let dir = std::env::temp_dir()
            .join(format!("pigide-memsvc-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = crate::db::open_in_memory().expect("open in-mem db");
        let ws_mgr = std::sync::Arc::new(WorkspaceManager::new(db.clone()));
        let ws = ws_mgr
            .create("phase0", vec![dir.to_string_lossy().to_string()])
            .expect("create ws");
        let svc = MemoryService::new(db, ws_mgr);
        (svc, ws.id, dir)
    }

    #[test]
    fn create_carries_kind_and_graph_exposes_it() {
        let (svc, ws_id, dir) = fresh_service();
        let n = svc
            .create(
                &ws_id,
                "Task ABC",
                "did the thing",
                vec!["auth".into()],
                vec![],
                Some("tasks/abc-123".into()),
                Kind::Task,
                None,
            )
            .unwrap();
        assert_eq!(n.kind, Kind::Task);
        assert_eq!(n.slug, "tasks/abc-123");
        let got = svc.get(&n.id).unwrap();
        assert_eq!(got.kind, Kind::Task);
        let g = svc.graph(&ws_id).unwrap();
        let node = g.nodes.iter().find(|x| x.id == n.id).unwrap();
        assert_eq!(node.kind, Kind::Task);
        std::fs::remove_dir_all(dir).ok();
    }
```

This test references signatures that don't exist yet (`create(..., kind, ingest)`, `GraphNode.kind`) — that's intentional.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p pigide memory::service::tests --lib create_carries_kind`
Expected: FAIL with compile error — `create` takes 6 args, test passes 8; `GraphNode` has no `kind` field.

- [ ] **Step 3: Extend `GraphNode` with `kind`**

In `src-tauri/src/memory/service.rs`, replace the `GraphNode` struct (lines 41-47) with:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: String,
    pub slug: String,
    pub title: String,
    pub kind: crate::memory::folders::Kind,
    pub tags: Vec<String>,
}
```

- [ ] **Step 4: Extend `create` to take `kind` + `ingest`**

Replace `create` (lines 80-106) with:

```rust
    pub fn create(
        &self,
        workspace_id: &str,
        title: &str,
        body: &str,
        tags: Vec<String>,
        aliases: Vec<String>,
        slug_override: Option<String>,
        kind: crate::memory::folders::Kind,
        ingest: Option<crate::memory::note::IngestRecord>,
    ) -> Result<Note> {
        if title.trim().is_empty() {
            return Err(Error::Invalid("title required".into()));
        }
        let root = self.root_for(workspace_id)?;
        let raw_slug = slug_override.unwrap_or_else(|| {
            // Default: prefix with the kind's folder so new notes from
            // ingest land in the right place automatically.
            format!("{}/{}", kind.folder(), storage::slugify(title))
        });
        let slug = self.unique_slug(&root.to_string_lossy(), raw_slug)?;
        let mut note = Note::new(slug.clone(), title.to_string(), body.to_string());
        note.kind = kind;
        note.tags = tags;
        note.aliases = aliases;
        note.ingest = ingest;
        let path = storage::slug_to_path(&root, &slug)?;
        let raw = note::serialize(&note);
        note::write(&path, &raw)?;
        self.upsert_index(&root.to_string_lossy(), &path, &note)?;
        self.rebuild_links(&note)?;
        Ok(note)
    }
```

- [ ] **Step 5: Update `unique_slug` to handle nested slugs without breaking on `-2` collisions**

Replace `unique_slug` (lines 108-125) with:

```rust
    fn unique_slug(&self, root_str: &str, base: String) -> Result<String> {
        let conn = self.db.get()?;
        let mut stmt =
            conn.prepare("SELECT 1 FROM memory_notes WHERE workspace_root=?1 AND slug=?2 LIMIT 1")?;
        let mut s = base.clone();
        let mut n: u32 = 2;
        loop {
            let exists: bool = stmt.exists([root_str, &s])?;
            if !exists {
                return Ok(s);
            }
            // Suffix the *last* segment so 'tasks/foo' becomes 'tasks/foo-2'
            // rather than 'tasks/foo-2' colliding with a literal slug
            // containing the slash.
            s = match base.rfind('/') {
                Some(idx) => format!("{}-{}", &base, n).replace(
                    &base[idx..],
                    &format!("{}-{}", &base[idx..], n),
                ),
                None => format!("{}-{}", base, n),
            };
            // Simpler: just suffix the whole string. The replace above is
            // equivalent for the no-slash case and produces a unique
            // suffix in either case because n is monotonic.
            s = format!("{}-{}", base, n);
            n += 1;
            if n > 999 {
                return Err(Error::Other("too many slug collisions".into()));
            }
        }
    }
```

> Pragmatic choice: a suffix on the full slug (`tasks/foo` → `tasks/foo-2`) is unambiguous because `validate_slug` rejects `//` and `tasks/foo-2` is a valid path. No need for the segment-aware version.

Actually clean that up — drop the unreachable assignment. Final form:

```rust
    fn unique_slug(&self, root_str: &str, base: String) -> Result<String> {
        let conn = self.db.get()?;
        let mut stmt =
            conn.prepare("SELECT 1 FROM memory_notes WHERE workspace_root=?1 AND slug=?2 LIMIT 1")?;
        let mut s = base.clone();
        let mut n: u32 = 2;
        loop {
            let exists: bool = stmt.exists([root_str, &s])?;
            if !exists {
                return Ok(s);
            }
            s = format!("{}-{}", base, n);
            n += 1;
            if n > 999 {
                return Err(Error::Other("too many slug collisions".into()));
            }
        }
    }
```

- [ ] **Step 6: Update `get`, `update`, `graph` to read/write `kind` + `ingest`**

In `get` (lines 212-234) — extend the SELECT and the `Note` constructor:

```rust
    pub fn get(&self, id: &str) -> Result<Note> {
        let conn = self.db.get()?;
        let mut stmt = conn.prepare(
            "SELECT id,slug,title,kind,tags_json,aliases_json,body,created_at,updated_at,ingest_json
             FROM memory_notes WHERE id=?1",
        )?;
        let mut rows = stmt.query([id])?;
        let row = rows
            .next()?
            .ok_or_else(|| Error::NotFound(format!("note {}", id)))?;
        let kind_str: String = row.get(3)?;
        let tags_json: String = row.get(4)?;
        let aliases_json: String = row.get(5)?;
        let ingest_json: Option<String> = row.get(9)?;
        Ok(Note {
            id: row.get(0)?,
            slug: row.get(1)?,
            title: row.get(2)?,
            kind: crate::memory::folders::Kind::parse(&kind_str)
                .unwrap_or_else(crate::memory::folders::Kind::default_for_legacy),
            tags: serde_json::from_str(&tags_json).unwrap_or_default(),
            aliases: serde_json::from_str(&aliases_json).unwrap_or_default(),
            body: row.get(6)?,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
            ingest: ingest_json.and_then(|s| serde_json::from_str(&s).ok()),
        })
    }
```

In `upsert_index` (lines 127-165) — extend the INSERT:

```rust
    fn upsert_index(&self, root_str: &str, path: &std::path::Path, note: &Note) -> Result<()> {
        let mtime = std::fs::metadata(path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let tags_json = serde_json::to_string(&note.tags)?;
        let aliases_json = serde_json::to_string(&note.aliases)?;
        let ingest_json = match &note.ingest {
            Some(i) => Some(serde_json::to_string(i)?),
            None => None,
        };
        let conn = self.db.get()?;
        conn.execute(
            "INSERT INTO memory_notes(id,workspace_root,slug,title,kind,path,tags_json,aliases_json,body,mtime,created_at,updated_at,ingest_json)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
             ON CONFLICT(id) DO UPDATE SET
                workspace_root=excluded.workspace_root,
                slug=excluded.slug,
                title=excluded.title,
                kind=excluded.kind,
                path=excluded.path,
                tags_json=excluded.tags_json,
                aliases_json=excluded.aliases_json,
                body=excluded.body,
                mtime=excluded.mtime,
                updated_at=excluded.updated_at,
                ingest_json=excluded.ingest_json",
            rusqlite::params![
                &note.id,
                root_str,
                &note.slug,
                &note.title,
                note.kind.as_str(),
                &path.to_string_lossy(),
                &tags_json,
                &aliases_json,
                &note.body,
                mtime,
                &note.created_at,
                &note.updated_at,
                &ingest_json,
            ],
        )?;
        Ok(())
    }
```

In `graph` (lines 462-497) — extend the node SELECT:

```rust
        let mut stmt_n = conn.prepare(
            "SELECT id, slug, title, kind, tags_json FROM memory_notes WHERE workspace_root=?1",
        )?;
        let nodes: Vec<GraphNode> = stmt_n
            .query_map([&root_str], |r| {
                let kind_str: String = r.get(3)?;
                let tags_json: String = r.get(4)?;
                Ok(GraphNode {
                    id: r.get(0)?,
                    slug: r.get(1)?,
                    title: r.get(2)?,
                    kind: crate::memory::folders::Kind::parse(&kind_str)
                        .unwrap_or_else(crate::memory::folders::Kind::default_for_legacy),
                    tags: serde_json::from_str(&tags_json).unwrap_or_default(),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
```

`update`, `delete`, `list`, `search`, `find_backlinks`, `suggest_connections`, `reindex_from_disk`, `delete_by_path` — **no signature change** required for Phase 0; they read whatever the index has.

- [ ] **Step 7: Run the new test (will still fail — DB schema lacks `kind`/`ingest_json`)**

Run: `cargo test -p pigide memory::service::tests --lib create_carries_kind`
Expected: FAIL with `no such column: kind` from rusqlite. That's the cue for Task 5.

- [ ] **Step 8: Commit (yes, with a failing test — the next task fixes it)**

Don't commit yet. Move to Task 5; we'll commit after the schema migration lands and the test goes green.

---

## Task 5: DB migration v16 — add `kind` + `ingest_json` columns

**Files:**
- Modify: `src-tauri/src/db.rs:48, 489-505` (bump target + new migration block)

- [ ] **Step 1: Bump the target version**

In `src-tauri/src/db.rs:48`, change:

```rust
    let target = 15;
```

to:

```rust
    let target = 16;
```

- [ ] **Step 2: Append the new migration block**

In `src-tauri/src/db.rs`, after the closing `}` of the `if current < 15 { ... }` block (around line 505) and **before** `conn.pragma_update(None, "user_version", target)?;`, insert:

```rust
    if current < 16 {
        // PigMemory Phase 0: add `kind` and `ingest_json` columns to
        // memory_notes. Existing rows get kind='source' so legacy notes
        // are still findable. ingest_json defaults NULL (only set for
        // notes written by the ingest pipeline).
        conn.execute_batch(
            "BEGIN;
             ALTER TABLE memory_notes ADD COLUMN kind TEXT NOT NULL DEFAULT 'source';
             ALTER TABLE memory_notes ADD COLUMN ingest_json TEXT;
             CREATE INDEX IF NOT EXISTS idx_notes_kind ON memory_notes(workspace_root, kind);
             COMMIT;",
        )?;
    }
```

- [ ] **Step 3: Run the service test**

Run: `cargo test -p pigide memory::service::tests --lib create_carries_kind`
Expected: PASS.

- [ ] **Step 4: Run all memory tests + the lib test suite**

Run: `cargo test -p pigide --lib memory`
Expected: PASS (all `memory::*` tests, including the new ones).

Run: `cargo test -p pigide --lib`
Expected: PASS.

- [ ] **Step 5: Commit Task 4 + Task 5 together (now coherent)**

```bash
git add src-tauri/src/memory/service.rs src-tauri/src/db.rs
git commit -m "feat(memory): thread kind+ingest through MemoryService

- DB migration v16: add kind TEXT NOT NULL DEFAULT 'source' and
  ingest_json TEXT to memory_notes, plus an index on (workspace_root,
  kind) for the kind-filter queries Phase 4 will need.
- create() now takes a Kind and an optional IngestRecord; default
  slug folds in the kind's folder ('tasks/abc' for Kind::Task).
- get() / upsert_index() / graph() read and write the new columns.
- GraphNode exposes kind so the frontend can colour by kind."
```

---

## Task 6: Update `memory::tools` callers (LLM-tool surface)

**Files:**
- Modify: `src-tauri/src/memory/tools.rs:122-160` (the `create_memory` and `update_memory` JSON entry points)

The smart-lane will call `create_memory` with a `kind` arg in Phase 2. We pre-wire it now so Phase 2 doesn't have to touch this file again.

- [ ] **Step 1: Read the current tool dispatch to find the exact extraction shape**

Run: `cat src-tauri/src/memory/tools.rs`

Identify how `args.get("title")`, `args.get("body")`, etc. are pulled. (The shape is `serde_json::Value`.)

- [ ] **Step 2: Add an optional `kind` arg to `create_memory`**

In the `"create_memory"` branch of the dispatch (around line 122), where `service.create(...)` is called, change the call site to:

```rust
        "create_memory" => {
            let title = args
                .get("title")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::Invalid("create_memory: title required".into()))?;
            let body = args
                .get("body")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let tags: Vec<String> = args
                .get("tags")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default();
            let aliases: Vec<String> = args
                .get("aliases")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default();
            let slug_override = args
                .get("slug")
                .and_then(|v| v.as_str())
                .map(String::from);
            let kind = args
                .get("kind")
                .and_then(|v| v.as_str())
                .and_then(crate::memory::folders::Kind::parse)
                .unwrap_or_else(crate::memory::folders::Kind::default_for_legacy);
            let note = service.create(
                &workspace_id,
                title,
                body,
                tags,
                aliases,
                slug_override,
                kind,
                None, // ingest is not exposed to the tool surface in Phase 0
            )?;
            Ok(serde_json::to_value(note)?)
        }
```

> **Note:** Match the existing extraction style in the file. If the current code uses different argument-extraction helpers (e.g. a typed `Args` struct), adapt accordingly — the **substance** is: read `kind` as an optional string, parse via `Kind::parse`, default to `Kind::default_for_legacy()`, pass through. If the existing branch already has its own argument-typed struct, add a `pub kind: Option<String>` field instead of inline parsing.

- [ ] **Step 3: Add `kind` to the tool's JSON schema declaration**

Find the JSON schema for `create_memory` (around line 10 — `"create_memory"` tool definition). Add to its `properties`:

```json
"kind": {
  "type": "string",
  "enum": ["concept", "entity", "source", "task", "chat", "meta"],
  "description": "Note kind. Defaults to 'source'. Folder placement follows the kind."
}
```

- [ ] **Step 4: Run the build**

Run: `cargo build -p pigide --lib`
Expected: build succeeds with no warnings about the new `kind` arg path. (If the existing `create_memory` body extracts args differently, fix the integration there.)

- [ ] **Step 5: Run all memory tests again**

Run: `cargo test -p pigide --lib memory`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/memory/tools.rs
git commit -m "feat(memory): expose kind on create_memory tool

LLM ingest in Phase 2 will pass kind: 'concept'|'entity'|... .
Defaults to 'source' for backward compat with existing callers."
```

---

## Task 7: One-shot disk migration for legacy `.pigmemory/` notes

**Files:**
- Create: `src-tauri/src/memory/migration.rs`
- Modify: `src-tauri/src/memory/mod.rs` (`pub mod migration;`)
- Modify: `src-tauri/src/lib.rs` (call `memory::migration::run_once` after DB migrations)

The DB column got a `DEFAULT 'source'` so the index is fine. But on-disk markdown files still lack `kind:` in their frontmatter. We re-serialize each one once. Idempotent: skip files that already have `kind:` in frontmatter.

- [ ] **Step 1: Create the migration module with a unit test**

Create `src-tauri/src/memory/migration.rs`:

```rust
//! One-shot Phase-0 disk migration.
//!
//! Walks every `.pigmemory/` root that the DB knows about and re-serializes
//! note files that lack a `kind:` frontmatter field. Idempotent: a second
//! invocation finds nothing to do. Errors per-file are logged and skipped
//! so a single bad file can't prevent startup.

use crate::db::DbPool;
use crate::error::Result;
use crate::memory::folders::{kind_for_slug, Kind};
use crate::memory::note;
use crate::memory::storage;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

const MIGRATION_KEY: &str = "memory.phase0.migrated";

/// Run the disk migration once per install. The completion flag is stored
/// in the `settings` KV table so re-runs are O(1).
pub fn run_once(db: &DbPool) -> Result<()> {
    if matches!(crate::db::get_setting(db, MIGRATION_KEY)?.as_deref(), Some("1")) {
        return Ok(());
    }
    let roots = collect_roots(db)?;
    let mut migrated = 0usize;
    let mut skipped = 0usize;
    for root in &roots {
        let (m, s) = migrate_root(root);
        migrated += m;
        skipped += s;
    }
    tracing::info!(migrated, skipped, roots = roots.len(), "memory phase0 disk migration done");
    crate::db::set_setting(db, MIGRATION_KEY, "1")?;
    Ok(())
}

fn collect_roots(db: &DbPool) -> Result<Vec<PathBuf>> {
    let conn = db.get()?;
    let mut stmt = conn.prepare("SELECT DISTINCT workspace_root FROM memory_notes")?;
    let roots: HashSet<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .collect();
    Ok(roots.into_iter().map(PathBuf::from).collect())
}

fn migrate_root(root: &Path) -> (usize, usize) {
    let mut migrated = 0;
    let mut skipped = 0;
    let walker = walk_md(root);
    for path in walker {
        match migrate_file(root, &path) {
            Ok(true) => migrated += 1,
            Ok(false) => skipped += 1,
            Err(e) => {
                tracing::warn!(path = %path.display(), "skip migrate: {}", e);
                skipped += 1;
            }
        }
    }
    (migrated, skipped)
}

fn walk_md(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(root, &mut out);
    out
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().and_then(|e| e.to_str()) == Some("md") {
            out.push(p);
        }
    }
}

fn migrate_file(root: &Path, path: &Path) -> Result<bool> {
    let raw = std::fs::read_to_string(path)?;
    if raw_has_kind(&raw) {
        return Ok(false);
    }
    let slug = match storage::path_to_slug(root, path) {
        Some(s) => s,
        None => return Ok(false),
    };
    let mut n = note::parse(&slug, &raw)?;
    n.kind = kind_for_slug(&slug);
    if n.kind == Kind::default_for_legacy() && !slug.contains('/') {
        // Flat legacy note: explicitly stamp Source so re-running is a no-op.
        n.kind = Kind::Source;
    }
    let serialized = note::serialize(&n);
    note::write(path, &serialized)?;
    Ok(true)
}

fn raw_has_kind(raw: &str) -> bool {
    let header = match raw.find("\n---") {
        Some(end) => &raw[..end],
        None => raw,
    };
    header
        .lines()
        .any(|l| l.trim_start().starts_with("kind:"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(label: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("{}-{}", label, uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn migrate_flat_legacy_note_stamps_source() {
        let root = tempdir("phase0-flat");
        let p = root.join("auth.md");
        std::fs::write(
            &p,
            "---\nid: 11111111-1111-1111-1111-111111111111\ntitle: Auth\ncreated_at: 2025-01-01T00:00:00Z\nupdated_at: 2025-01-01T00:00:00Z\n---\nbody\n",
        )
        .unwrap();
        let changed = migrate_file(&root, &p).unwrap();
        assert!(changed);
        let after = std::fs::read_to_string(&p).unwrap();
        assert!(after.contains("kind: source"));
        // Idempotent: second pass returns false.
        assert!(!migrate_file(&root, &p).unwrap());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn migrate_nested_legacy_note_picks_kind_from_folder() {
        let root = tempdir("phase0-nested");
        std::fs::create_dir_all(root.join("tasks")).unwrap();
        let p = root.join("tasks").join("abc.md");
        std::fs::write(
            &p,
            "---\nid: 22222222-2222-2222-2222-222222222222\ntitle: T\ncreated_at: 2025-01-01T00:00:00Z\nupdated_at: 2025-01-01T00:00:00Z\n---\nbody\n",
        )
        .unwrap();
        let changed = migrate_file(&root, &p).unwrap();
        assert!(changed);
        let after = std::fs::read_to_string(&p).unwrap();
        assert!(after.contains("kind: task"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn migrate_already_migrated_file_is_noop() {
        let root = tempdir("phase0-already");
        let p = root.join("x.md");
        std::fs::write(
            &p,
            "---\nid: 33333333-3333-3333-3333-333333333333\ntitle: X\nkind: concept\ncreated_at: 2025-01-01T00:00:00Z\nupdated_at: 2025-01-01T00:00:00Z\n---\nbody\n",
        )
        .unwrap();
        let before = std::fs::read_to_string(&p).unwrap();
        assert!(!migrate_file(&root, &p).unwrap());
        let after = std::fs::read_to_string(&p).unwrap();
        assert_eq!(before, after);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn raw_has_kind_detects_present() {
        assert!(raw_has_kind("---\nid: 1\nkind: concept\n---\nbody"));
        assert!(!raw_has_kind("---\nid: 1\ntitle: x\n---\nbody"));
    }
}
```

- [ ] **Step 2: Wire it up in `mod.rs`**

In `src-tauri/src/memory/mod.rs`, add `pub mod migration;` so the file becomes:

```rust
pub mod folders;
pub mod links;
pub mod migration;
pub mod note;
pub mod service;
pub mod storage;
pub mod tools;
pub mod watcher;

pub use service::MemoryService;
```

- [ ] **Step 3: Run the new tests**

Run: `cargo test -p pigide memory::migration::tests --lib`
Expected: PASS — 4 tests.

- [ ] **Step 4: Hook the migration into startup**

Find the place in `src-tauri/src/lib.rs` where `db::run_migrations` is called. Right after that call (and before any code that reads notes), call `crate::memory::migration::run_once(&db)?;`.

Search command to locate the spot:

```bash
grep -n "run_migrations\|memory_root\|MemoryService::new" src-tauri/src/lib.rs
```

Insert the call directly after the DB migration line, e.g.:

```rust
    crate::db::run_migrations(&db)?;
    crate::memory::migration::run_once(&db)?;
```

- [ ] **Step 5: Build and run the full lib test suite**

Run: `cargo build -p pigide --lib`
Expected: success.

Run: `cargo test -p pigide --lib`
Expected: PASS for everything in the workspace.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/memory/migration.rs src-tauri/src/memory/mod.rs src-tauri/src/lib.rs
git commit -m "feat(memory): one-shot Phase 0 disk migration

Walks every known .pigmemory/ root once at startup and stamps a
kind: line into the frontmatter of every legacy note file. Picks
kind from the folder prefix (tasks/, concepts/, ...) when present;
flat slugs fall back to Source. Completion flag stored in settings
KV so repeat startups are O(1)."
```

---

## Task 8: Frontend types — add `Kind` to `Note`/`GraphNode`

**Files:**
- Modify: `frontend/src/state/types.ts`

The IPC contract now returns `kind` on every note + graph node. Add the type so TypeScript strict mode doesn't complain when Phase 4 starts using it.

- [ ] **Step 1: Locate the existing types**

Run:

```bash
grep -n "interface Note\|interface NoteSummary\|interface GraphNode" frontend/src/state/types.ts
```

- [ ] **Step 2: Add `NoteKind` and extend the three types**

Add at the top of the relevant section in `frontend/src/state/types.ts`:

```typescript
export type NoteKind =
  | "concept"
  | "entity"
  | "source"
  | "task"
  | "chat"
  | "meta";
```

Then in each of `Note`, `NoteSummary`, `GraphNode`, add the field (use `?` for `NoteSummary` and `GraphNode` so old responses don't break the type — the backend will always send it post-migration but the optional marker keeps the change non-breaking for any not-yet-rebuilt branch):

```typescript
  kind?: NoteKind;
```

For the full `Note` type (which the editor reads), make it required:

```typescript
  kind: NoteKind;
```

And add the `ingest` field to `Note`:

```typescript
  ingest?: {
    source_kind: string;
    source_ref?: string;
    ingested_at: string;
    smart_pass_at?: string;
  };
```

- [ ] **Step 3: Run the frontend type-check**

Run: `cd frontend && npm run typecheck`
Expected: PASS. (If it fails, fix the call sites — most likely 1-2 places in `PigMemoryWorkbench.tsx` that destructure `Note` will need the field added or marked optional.)

- [ ] **Step 4: Run the frontend build**

Run: `cd frontend && npm run build`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add frontend/src/state/types.ts
git commit -m "feat(memory): NoteKind + ingest in TS types

Mirrors the Rust-side Kind enum and IngestRecord. Phase 4 will
start using these in the graph for colour-by-kind."
```

---

## Task 9: Smoke verification

**Files:** none (manual + automated checks)

- [ ] **Step 1: Run the full test suite**

Run: `cargo test -p pigide`
Expected: PASS.

Run: `cd frontend && npm run typecheck && npm run build`
Expected: PASS.

- [ ] **Step 2: Boot the app once against a real workspace and confirm migration log line**

Run: `cd src-tauri && cargo run` (or whatever the project's dev entry is — typically `npm run tauri dev` from repo root).

In the log output, confirm a line resembling:

```
INFO memory phase0 disk migration done migrated=N skipped=M roots=K
```

If `K==0` you have no existing `.pigmemory/` dirs — that's fine.

- [ ] **Step 3: Inspect one migrated note (if any existed)**

If you have legacy notes:

```bash
ls ~/some-workspace-with-pigmemory/.pigmemory/*.md | head -1 | xargs head -10
```

Expected: see a `kind: source` line in the frontmatter.

- [ ] **Step 4: Open PigMemory tab in the UI**

Notes still load, graph still renders, search still works. No visible behavior change. Done.

---

## Self-Review

**1. Spec coverage** — checked § 4 of spec:
- ✅ Allow `/` in slug — Task 1
- ✅ `kind` in frontmatter — Task 3
- ✅ `ingest` block in frontmatter — Task 3
- ✅ One-shot migration, idempotent — Task 7
- ✅ Frontend types — Task 8
- ✅ DB schema for `kind`/`ingest_json` — Task 5
- ✅ Folder mapping centralised — Task 2
- ✅ Tool surface ready for Phase 2 — Task 6

No gaps. Phases 1–6 build on this; nothing in Phase 0 spec is left unimplemented.

**2. Placeholder scan** — searched for "TBD", "TODO", "implement later", "similar to". One soft note in Task 6 Step 2 ("Match the existing extraction style ... adapt accordingly") — this is a real instruction, not a placeholder. Kept because the existing tools.rs argument shape isn't fully shown in the plan and the engineer must reconcile with what's actually there. The substance (parse Kind, default to legacy, pass through) is fully spelled out.

**3. Type consistency** — `Kind` enum is the single source of truth across Rust (`folders.rs`) and TS (`NoteKind`). `IngestRecord` fields match between Rust struct (Task 3) and TS interface (Task 8): `source_kind`, `source_ref`, `ingested_at`, `smart_pass_at`. `kind` column type matches between DB migration (`TEXT NOT NULL DEFAULT 'source'`, Task 5) and Rust write/read (`note.kind.as_str()`, Task 4). `unique_slug` uses `format!("{}-{}", base, n)` consistently in the final form.

No issues found.
