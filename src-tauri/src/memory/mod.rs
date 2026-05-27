//! PigMemory: local-first markdown notes with [[wikilinks]], FTS5 search,
//! backlinks, and BM25-based "suggest_connections".
//!
//! Storage layout: `<workspace_root>/.pigmemory/<slug>.md`. Slugs may include
//! `/` for nested folders. Each note carries a YAML frontmatter with a stable
//! `id` (uuid v4) — the path/slug is secondary so renames don't break links.

pub mod folders;
pub mod links;
pub mod note;
pub mod service;
pub mod storage;
pub mod tools;
pub mod watcher;

pub use service::MemoryService;
