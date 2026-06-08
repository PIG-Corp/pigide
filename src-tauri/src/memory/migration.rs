//! One-shot Phase-0 disk migration.
//!
//! Walks every `.pigmemory/` root that the DB knows about and re-serializes
//! note files that lack a `kind:` frontmatter field. Idempotent: a second
//! invocation finds nothing to do. Errors per-file are logged and skipped
//! so a single bad file can't prevent startup.
//!
//! Also backfills the DB `kind` column from the inferred kind, so notes
//! whose on-disk frontmatter said `kind: task` (but whose DB row was
//! defaulted to 'source' by migration v16) end up consistent.

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
    if matches!(
        crate::db::get_setting(db, MIGRATION_KEY)?.as_deref(),
        Some("1")
    ) {
        return Ok(());
    }
    let roots = collect_roots(db)?;
    let mut migrated = 0usize;
    let mut skipped = 0usize;
    for root in &roots {
        let (m, s) = migrate_root(db, root);
        migrated += m;
        skipped += s;
    }
    tracing::info!(
        migrated,
        skipped,
        roots = roots.len(),
        "memory phase0 disk migration done"
    );
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

fn migrate_root(db: &DbPool, root: &Path) -> (usize, usize) {
    let mut migrated = 0;
    let mut skipped = 0;
    let walker = walk_md(root);
    for path in walker {
        match migrate_file(db, root, &path) {
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
        // Skip symlinks: a symlink planted inside `.pigmemory/` could
        // otherwise make the migrator read and rewrite a `.md` file
        // anywhere on disk the user can reach. `file_type()` does not
        // follow the link, so this is the no-follow check.
        if entry.file_type().map(|t| t.is_symlink()).unwrap_or(true) {
            continue;
        }
        let p = entry.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().and_then(|e| e.to_str()) == Some("md") {
            out.push(p);
        }
    }
}

/// Migrate a single file. Returns Ok(true) if the file was rewritten,
/// Ok(false) if no work was needed (already has `kind:` in frontmatter
/// or no resolvable slug). Also backfills the DB `kind` column to the
/// inferred kind so the index agrees with disk after this pass.
fn migrate_file(db: &DbPool, root: &Path, path: &Path) -> Result<bool> {
    let raw = std::fs::read_to_string(path)?;
    let already_has_kind = raw_has_kind(&raw);
    let slug = match storage::path_to_slug(root, path) {
        Some(s) => s,
        None => return Ok(false),
    };
    let inferred = kind_for_slug(&slug);
    backfill_db_kind(db, path, inferred)?;
    if already_has_kind {
        return Ok(false);
    }
    let mut n = note::parse(&slug, &raw)?;
    n.kind = inferred;
    let serialized = note::serialize(&n);
    note::write(path, &serialized)?;
    Ok(true)
}

fn backfill_db_kind(db: &DbPool, path: &Path, kind: Kind) -> Result<()> {
    let conn = db.get()?;
    conn.execute(
        "UPDATE memory_notes SET kind=?1 WHERE path=?2",
        rusqlite::params![kind.as_str(), &path.to_string_lossy()],
    )?;
    Ok(())
}

fn raw_has_kind(raw: &str) -> bool {
    let header = match raw.find("\n---") {
        Some(end) => &raw[..end],
        None => raw,
    };
    header.lines().any(|l| l.trim_start().starts_with("kind:"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(label: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("{}-{}", label, uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn fresh_pool() -> DbPool {
        let manager = r2d2_sqlite::SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder()
            .max_size(1)
            .build(manager)
            .expect("pool");
        let conn = pool.get().expect("conn");
        crate::db::migrate_one(&conn).expect("migrate");
        drop(conn);
        pool
    }

    fn insert_row(db: &DbPool, root: &Path, path: &Path, slug: &str) {
        let conn = db.get().unwrap();
        conn.execute(
            "INSERT INTO memory_notes(id,workspace_root,slug,title,kind,path,tags_json,aliases_json,body,mtime,created_at,updated_at,ingest_json)
             VALUES(?1,?2,?3,?4,'source',?5,'[]','[]','body',0,'2025-01-01T00:00:00Z','2025-01-01T00:00:00Z',NULL)",
            rusqlite::params![
                uuid::Uuid::new_v4().to_string(),
                root.to_string_lossy(),
                slug,
                "X",
                path.to_string_lossy(),
            ],
        ).unwrap();
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
        let db = fresh_pool();
        insert_row(&db, &root, &p, "auth");
        let changed = migrate_file(&db, &root, &p).unwrap();
        assert!(changed);
        let after = std::fs::read_to_string(&p).unwrap();
        assert!(after.contains("kind: source"));
        // DB row still 'source' (the row started 'source' and inferred is also 'source').
        let kind: String = db
            .get()
            .unwrap()
            .query_row(
                "SELECT kind FROM memory_notes WHERE path=?1",
                rusqlite::params![p.to_string_lossy()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kind, "source");
        // Idempotent: second pass returns false.
        assert!(!migrate_file(&db, &root, &p).unwrap());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn migrate_nested_legacy_note_picks_kind_from_folder_and_backfills_db() {
        let root = tempdir("phase0-nested");
        std::fs::create_dir_all(root.join("tasks")).unwrap();
        let p = root.join("tasks").join("abc.md");
        std::fs::write(
            &p,
            "---\nid: 22222222-2222-2222-2222-222222222222\ntitle: T\ncreated_at: 2025-01-01T00:00:00Z\nupdated_at: 2025-01-01T00:00:00Z\n---\nbody\n",
        )
        .unwrap();
        let db = fresh_pool();
        insert_row(&db, &root, &p, "tasks/abc");
        let changed = migrate_file(&db, &root, &p).unwrap();
        assert!(changed);
        let after = std::fs::read_to_string(&p).unwrap();
        assert!(after.contains("kind: task"));
        // DB backfill: row should now be 'task'.
        let kind: String = db
            .get()
            .unwrap()
            .query_row(
                "SELECT kind FROM memory_notes WHERE path=?1",
                rusqlite::params![p.to_string_lossy()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kind, "task");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn migrate_already_migrated_file_is_noop_but_still_backfills_db() {
        let root = tempdir("phase0-already");
        std::fs::create_dir_all(root.join("concepts")).unwrap();
        let p = root.join("concepts").join("x.md");
        std::fs::write(
            &p,
            "---\nid: 33333333-3333-3333-3333-333333333333\ntitle: X\nkind: concept\ncreated_at: 2025-01-01T00:00:00Z\nupdated_at: 2025-01-01T00:00:00Z\n---\nbody\n",
        )
        .unwrap();
        let db = fresh_pool();
        // DB row defaulted to 'source' even though disk says 'concept'.
        insert_row(&db, &root, &p, "concepts/x");
        let before_disk = std::fs::read_to_string(&p).unwrap();
        let changed = migrate_file(&db, &root, &p).unwrap();
        assert!(!changed); // file unchanged
        let after_disk = std::fs::read_to_string(&p).unwrap();
        assert_eq!(before_disk, after_disk);
        // But DB was backfilled to 'concept' from kind_for_slug.
        let kind: String = db
            .get()
            .unwrap()
            .query_row(
                "SELECT kind FROM memory_notes WHERE path=?1",
                rusqlite::params![p.to_string_lossy()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kind, "concept");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn raw_has_kind_detects_present() {
        assert!(raw_has_kind("---\nid: 1\nkind: concept\n---\nbody"));
        assert!(!raw_has_kind("---\nid: 1\ntitle: x\n---\nbody"));
    }

    #[test]
    fn run_once_is_idempotent() {
        let db = fresh_pool();
        run_once(&db).unwrap();
        // Second call is a no-op (no rows in DB, but the flag should be set).
        let flag = crate::db::get_setting(&db, MIGRATION_KEY).unwrap();
        assert_eq!(flag.as_deref(), Some("1"));
        run_once(&db).unwrap(); // should return immediately, no panic
    }
}
