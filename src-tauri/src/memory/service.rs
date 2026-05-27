//! MemoryService: write-through layer between markdown files on disk and
//! SQLite index (`memory_notes`, `memory_links`, `memory_fts`).

use crate::db::DbPool;
use crate::error::{Error, Result};
use crate::memory::links::{self, WikiRef};
use crate::memory::note::{self, Note};
use crate::memory::storage;
use crate::workspace::WorkspaceManager;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoteSummary {
    pub id: String,
    pub slug: String,
    pub title: String,
    pub tags: Vec<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub id: String,
    pub slug: String,
    pub title: String,
    pub snippet: String,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Backlink {
    pub src_id: String,
    pub src_slug: String,
    pub src_title: String,
    pub context: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: String,
    pub slug: String,
    pub title: String,
    pub kind: crate::memory::folders::Kind,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdge {
    pub source: String,
    pub target: Option<String>,
    pub target_text: String,
    pub ambiguous: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphData {
    pub nodes: Vec<GraphNode>,
    pub links: Vec<GraphEdge>,
}

pub struct MemoryService {
    db: DbPool,
    ws_mgr: Arc<WorkspaceManager>,
}

impl MemoryService {
    pub fn new(db: DbPool, ws_mgr: Arc<WorkspaceManager>) -> Self {
        Self { db, ws_mgr }
    }

    /// Resolve the storage root for a workspace, ensuring it exists.
    fn root_for(&self, workspace_id: &str) -> Result<PathBuf> {
        let root = storage::resolve_root(&self.ws_mgr, workspace_id)?;
        storage::ensure_root(&root)?;
        Ok(root)
    }

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

    fn rebuild_links(&self, note: &Note) -> Result<()> {
        let refs: Vec<WikiRef> = links::extract(&note.body);
        let conn = self.db.get()?;
        conn.execute("DELETE FROM memory_links WHERE src_id=?1", [&note.id])?;
        if refs.is_empty() {
            return Ok(());
        }
        // Pull all candidates from the same workspace_root for resolution.
        let root: String = conn.query_row(
            "SELECT workspace_root FROM memory_notes WHERE id=?1",
            [&note.id],
            |r| r.get(0),
        )?;
        let mut stmt = conn.prepare(
            "SELECT id, slug, title, aliases_json FROM memory_notes WHERE workspace_root=?1",
        )?;
        let candidates: Vec<links::Candidate> = stmt
            .query_map([&root], |r| {
                let aliases_json: String = r.get(3)?;
                Ok(links::Candidate {
                    id: r.get(0)?,
                    slug: r.get(1)?,
                    title: r.get(2)?,
                    aliases: serde_json::from_str(&aliases_json).unwrap_or_default(),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut insert = conn.prepare(
            "INSERT OR REPLACE INTO memory_links(src_id, dst_id, dst_text, display, ambiguous)
             VALUES(?1, ?2, ?3, ?4, ?5)",
        )?;
        for r in &refs {
            let (dst_id, ambiguous) = match links::resolve(&r.target, &candidates) {
                links::Resolution::Resolved { id } => (Some(id), 0),
                links::Resolution::Ambiguous { ids } => (ids.into_iter().next(), 1),
                links::Resolution::Unresolved => (None, 0),
            };
            insert.execute(rusqlite::params![
                &note.id, &dst_id, &r.target, &r.display, ambiguous
            ])?;
        }
        Ok(())
    }

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

    pub fn update(
        &self,
        id: &str,
        title: Option<String>,
        body: Option<String>,
        tags: Option<Vec<String>>,
        aliases: Option<Vec<String>>,
    ) -> Result<Note> {
        let mut note = self.get(id)?;
        if let Some(t) = title {
            note.title = t;
        }
        if let Some(b) = body {
            note.body = b;
        }
        if let Some(t) = tags {
            note.tags = t;
        }
        if let Some(a) = aliases {
            note.aliases = a;
        }
        note.updated_at = Utc::now().to_rfc3339();
        // Re-write file on disk and re-index.
        let conn = self.db.get()?;
        let (root_str, path_str): (String, String) = conn.query_row(
            "SELECT workspace_root, path FROM memory_notes WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let path =
            crate::files::validate_workspace_write_path(&path_str, &[PathBuf::from(&root_str)])?;
        let raw = note::serialize(&note);
        note::write(&path, &raw)?;
        self.upsert_index(&root_str, &path, &note)?;
        self.rebuild_links(&note)?;
        Ok(note)
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        let conn = self.db.get()?;
        let stored_path: Option<(String, String)> = conn
            .query_row(
                "SELECT workspace_root, path FROM memory_notes WHERE id=?1",
                [id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .ok();
        conn.execute("DELETE FROM memory_notes WHERE id=?1", [id])?;
        if let Some((root, path)) = stored_path {
            match crate::files::validate_existing_workspace_path(&path, &[PathBuf::from(root)]) {
                Ok(p) => {
                    let _ = std::fs::remove_file(p);
                }
                Err(e) => {
                    tracing::warn!(note_id = %id, "skipping unsafe memory file delete: {}", e);
                }
            }
        }
        Ok(())
    }

    pub fn list(
        &self,
        workspace_id: &str,
        tag: Option<&str>,
        limit: i64,
    ) -> Result<Vec<NoteSummary>> {
        let root = self.root_for(workspace_id)?;
        let root_str = root.to_string_lossy().to_string();
        let conn = self.db.get()?;
        let mut stmt = conn.prepare(
            "SELECT id, slug, title, tags_json, updated_at
             FROM memory_notes
             WHERE workspace_root=?1
             ORDER BY updated_at DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![&root_str, limit.clamp(1, 500)], |r| {
            let tags_json: String = r.get(3)?;
            let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
            Ok(NoteSummary {
                id: r.get(0)?,
                slug: r.get(1)?,
                title: r.get(2)?,
                tags,
                updated_at: r.get(4)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            let s = row?;
            if let Some(filter_tag) = tag {
                if !s.tags.iter().any(|t| t == filter_tag) {
                    continue;
                }
            }
            out.push(s);
        }
        Ok(out)
    }

    pub fn search(&self, workspace_id: &str, query: &str, limit: i64) -> Result<Vec<SearchHit>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let root = self.root_for(workspace_id)?;
        let root_str = root.to_string_lossy().to_string();
        let q = sanitize_fts_query(query);
        let conn = self.db.get()?;
        let mut stmt = conn.prepare(
            "SELECT n.id, n.slug, n.title,
                    snippet(memory_fts, 1, '<<', '>>', '…', 16),
                    bm25(memory_fts, 4.0, 1.0, 2.0, 1.5)
             FROM memory_fts f
             JOIN memory_notes n ON n.rowid = f.rowid
             WHERE n.workspace_root=?1 AND memory_fts MATCH ?2
             ORDER BY bm25(memory_fts, 4.0, 1.0, 2.0, 1.5)
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(rusqlite::params![&root_str, &q, limit.clamp(1, 50)], |r| {
            Ok(SearchHit {
                id: r.get(0)?,
                slug: r.get(1)?,
                title: r.get(2)?,
                snippet: r.get(3)?,
                // bm25 returns negative scores (lower = better). Flip.
                score: -r.get::<_, f64>(4)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn find_backlinks(&self, id: &str) -> Result<Vec<Backlink>> {
        let conn = self.db.get()?;
        let mut stmt = conn.prepare(
            "SELECT n.id, n.slug, n.title, n.body
             FROM memory_links l
             JOIN memory_notes n ON n.id = l.src_id
             WHERE l.dst_id = ?1",
        )?;
        let rows = stmt.query_map([id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (src_id, src_slug, src_title, body) = row?;
            let context = extract_context(&body, id, &self.db).unwrap_or_default();
            out.push(Backlink {
                src_id,
                src_slug,
                src_title,
                context,
            });
        }
        Ok(out)
    }

    pub fn suggest_connections(&self, id: &str, limit: i64) -> Result<Vec<SearchHit>> {
        // Pull title + first 2k chars of body as the implicit query.
        let note = self.get(id)?;
        let mut q = note.title.clone();
        q.push(' ');
        q.push_str(&note.body.chars().take(2000).collect::<String>());
        let root = match self.note_root(id)? {
            Some(r) => r,
            None => return Ok(Vec::new()),
        };
        let mut q_clean = sanitize_fts_query(&q);
        if q_clean == "x" {
            q_clean = sanitize_fts_query(&note.title);
            if q_clean == "x" {
                return Ok(Vec::new());
            }
        }
        let conn = self.db.get()?;
        let mut stmt = conn.prepare(
            "SELECT n.id, n.slug, n.title,
                    snippet(memory_fts, 1, '<<', '>>', '…', 16),
                    bm25(memory_fts, 4.0, 1.0, 2.0, 1.5),
                    n.tags_json
             FROM memory_fts f
             JOIN memory_notes n ON n.rowid = f.rowid
             WHERE n.workspace_root=?1 AND n.id != ?2 AND memory_fts MATCH ?3
             ORDER BY bm25(memory_fts, 4.0, 1.0, 2.0, 1.5)
             LIMIT ?4",
        )?;
        let take = limit.clamp(1, 20);
        let self_tags: std::collections::HashSet<String> = note.tags.into_iter().collect();
        let rows = stmt.query_map(rusqlite::params![&root, id, &q_clean, take], |r| {
            let tags_json: String = r.get(5)?;
            let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
            Ok((
                SearchHit {
                    id: r.get(0)?,
                    slug: r.get(1)?,
                    title: r.get(2)?,
                    snippet: r.get(3)?,
                    score: -r.get::<_, f64>(4)?,
                },
                tags,
            ))
        })?;
        let mut hits: Vec<SearchHit> = Vec::new();
        for row in rows {
            let (mut h, tags) = row?;
            let overlap = tags.iter().filter(|t| self_tags.contains(*t)).count() as f64;
            h.score += 0.3 * overlap;
            hits.push(h);
        }
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(hits)
    }

    pub fn graph(&self, workspace_id: &str) -> Result<GraphData> {
        let root = self.root_for(workspace_id)?;
        let root_str = root.to_string_lossy().to_string();
        let conn = self.db.get()?;
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
        let mut stmt_e = conn.prepare(
            "SELECT l.src_id, l.dst_id, l.dst_text, l.ambiguous
             FROM memory_links l
             JOIN memory_notes n ON n.id = l.src_id
             WHERE n.workspace_root=?1",
        )?;
        let links: Vec<GraphEdge> = stmt_e
            .query_map([&root_str], |r| {
                Ok(GraphEdge {
                    source: r.get(0)?,
                    target: r.get(1)?,
                    target_text: r.get(2)?,
                    ambiguous: r.get::<_, i64>(3)? != 0,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(GraphData { nodes, links })
    }

    fn note_root(&self, id: &str) -> Result<Option<String>> {
        let conn = self.db.get()?;
        Ok(conn
            .query_row(
                "SELECT workspace_root FROM memory_notes WHERE id=?1",
                [id],
                |r| r.get::<_, String>(0),
            )
            .ok())
    }

    /// Re-index a note that just changed on disk (called by the watcher).
    /// Skips when the on-disk mtime matches the indexed value (cheap idempotent guard).
    pub fn reindex_from_disk(
        &self,
        _workspace_id: &str,
        root: &std::path::Path,
        path: &std::path::Path,
        mut note: Note,
    ) -> Result<()> {
        let on_disk_mtime = std::fs::metadata(path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let conn = self.db.get()?;
        let prev: Option<i64> = conn
            .query_row(
                "SELECT mtime FROM memory_notes WHERE id=?1",
                [&note.id],
                |r| r.get(0),
            )
            .ok();
        if matches!(prev, Some(m) if m >= on_disk_mtime) {
            return Ok(());
        }
        // Bump updated_at to wall-clock; preserve id from frontmatter.
        note.updated_at = Utc::now().to_rfc3339();
        self.upsert_index(&root.to_string_lossy(), path, &note)?;
        self.rebuild_links(&note)?;
        Ok(())
    }

    /// Delete a note when its file disappears on disk.
    pub fn delete_by_path(&self, path: &std::path::Path) -> Result<()> {
        let conn = self.db.get()?;
        let path_str = path.to_string_lossy().to_string();
        let id: Option<String> = conn
            .query_row(
                "SELECT id FROM memory_notes WHERE path=?1",
                [&path_str],
                |r| r.get::<_, String>(0),
            )
            .ok();
        if let Some(id) = id {
            conn.execute("DELETE FROM memory_notes WHERE id=?1", [&id])?;
        }
        Ok(())
    }
}

fn extract_context(body: &str, _dst_id: &str, _pool: &DbPool) -> Option<String> {
    // Take the first 160 chars around the first wikilink as a cheap context.
    let first = body.find("[[")?;
    let end = body[first..]
        .find("]]")
        .map(|e| first + e + 2)
        .unwrap_or(body.len());
    let lo = body.floor_char_boundary(first.saturating_sub(80));
    let hi = body.floor_char_boundary((end + 80).min(body.len()));
    Some(body[lo..hi].replace('\n', " "))
}

/// Defensive sanitization for FTS5 MATCH input. Strips operator chars that
/// could otherwise produce a syntax error, then OR-joins remaining tokens.
fn sanitize_fts_query(q: &str) -> String {
    let cleaned: String = q
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' {
                c
            } else {
                ' '
            }
        })
        .collect();

    let mut positives = Vec::new();
    let mut negatives = Vec::new();
    let mut has_positive = false;

    for raw_tok in cleaned.split_whitespace() {
        let trimmed = raw_tok.trim_matches('-');
        if trimmed.is_empty() {
            continue;
        }

        if raw_tok.starts_with('-') {
            if has_positive {
                for part in trimmed.split('-') {
                    if part.len() >= 2 {
                        negatives.push(part.to_lowercase());
                    }
                }
            } else {
                for part in trimmed.split('-') {
                    if part.len() >= 2 {
                        positives.push(part.to_lowercase());
                        has_positive = true;
                    }
                }
            }
        } else {
            for part in raw_tok.split('-') {
                if part.len() >= 2 {
                    positives.push(part.to_lowercase());
                    has_positive = true;
                }
            }
        }
    }

    if positives.is_empty() {
        "x".to_string()
    } else {
        let pos_str = positives.join(" OR ");
        if negatives.is_empty() {
            pos_str
        } else {
            format!("{} NOT {}", pos_str, negatives.join(" NOT "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_operators() {
        let q = sanitize_fts_query("hello: \"world\" AND go!");
        assert!(q.contains(" OR "));
        assert!(!q.contains('"'));
        assert!(!q.contains(':'));
    }

    #[test]
    fn sanitize_handles_empty() {
        assert_eq!(sanitize_fts_query(""), "x");
    }

    #[test]
    fn sanitize_strips_hyphen_not_operator() {
        let q = sanitize_fts_query("-test");
        assert_eq!(q, "test");
    }

    #[test]
    fn sanitize_strips_leading_hyphen_tokens() {
        let q = sanitize_fts_query("hello -world --backend");
        assert_eq!(q, "hello NOT world NOT backend");
    }

    #[test]
    fn sanitize_preserves_underscores() {
        let q = sanitize_fts_query("my_func other_thing");
        assert_eq!(q, "my_func OR other_thing");
    }

    use crate::memory::folders::Kind;
    use crate::workspace::WorkspaceManager;

    fn fresh_service() -> (MemoryService, String, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("pigide-memsvc-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let manager = r2d2_sqlite::SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(1).build(manager).unwrap();
        {
            let conn = pool.get().unwrap();
            crate::db::migrate_one(&conn).expect("migrate in-mem db");
        }
        let ws_mgr = std::sync::Arc::new(WorkspaceManager::new(pool.clone()));
        let ws = ws_mgr
            .create("phase0", vec![dir.to_string_lossy().to_string()])
            .expect("create ws");
        let svc = MemoryService::new(pool, ws_mgr);
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
}
