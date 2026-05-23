//! Stateful façade around the indexer + resolver.
//!
//! Holds the in-memory index, refreshes it on demand, and exposes a
//! single `resolve` entrypoint to the rest of the app.

use crate::db::DbPool;
use crate::error::{Error, Result};
use crate::project_resolver::aliases;
use crate::project_resolver::indexer::{self, ProjectEntry, ProjectIndex, ScanOptions};
use crate::project_resolver::resolver::{self, ResolveContext, ResolveOutcome};
use crate::workspace::WorkspaceManager;
use parking_lot::RwLock;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const TTL: Duration = Duration::from_secs(24 * 60 * 60);

pub struct ResolverService {
    db: DbPool,
    ws_mgr: Arc<WorkspaceManager>,
    index: RwLock<Option<ProjectIndex>>,
    last_built_wall: RwLock<Option<Instant>>,
}

impl ResolverService {
    pub fn new(db: DbPool, ws_mgr: Arc<WorkspaceManager>) -> Self {
        let me = Self {
            db,
            ws_mgr,
            index: RwLock::new(None),
            last_built_wall: RwLock::new(None),
        };
        if let Some(cached) = indexer::load_cache() {
            *me.index.write() = Some(cached);
        }
        me
    }

    /// User-configured extra roots from the `project_resolver.roots` setting
    /// (JSON array of absolute paths).
    fn extra_roots(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();

        if let Ok(Some(raw)) = crate::db::get_setting(&self.db, "project_resolver.roots") {
            if let Ok(arr) = serde_json::from_str::<Vec<String>>(&raw) {
                for p in arr {
                    out.push(PathBuf::from(p));
                }
            }
        }

        // Workspace store paths get implicitly indexed too.
        if let Ok(list) = self.ws_mgr.list() {
            for ws in list {
                for p in ws.paths {
                    out.push(PathBuf::from(p));
                }
            }
        }
        out
    }

    /// Force-rebuild the index from disk.
    pub fn rebuild(&self) -> ProjectIndex {
        let extra = self.extra_roots();
        let opts = ScanOptions {
            roots: indexer::default_roots(&extra),
            ..Default::default()
        };
        self.rebuild_with_options(opts)
    }

    /// Rebuild from explicit options. Bypasses the default `$HOME` sniffing —
    /// useful for tests and "rescan only this directory" UX flows.
    pub fn rebuild_with_options(&self, opts: ScanOptions) -> ProjectIndex {
        let idx = indexer::scan(opts);
        let _ = indexer::save_cache(&idx);
        *self.index.write() = Some(idx.clone());
        *self.last_built_wall.write() = Some(Instant::now());
        idx
    }

    /// Return the in-memory index, building one (and caching to disk) on
    /// first use. Stale-but-readable cache is served immediately;
    /// callers that want strict freshness can call `rebuild`.
    pub fn ensure(&self) -> ProjectIndex {
        if let Some(idx) = self.index.read().clone() {
            if indexer::is_fresh(&idx, TTL) {
                return idx;
            }
        }
        self.rebuild()
    }

    /// Resolve a query. Pulls recent workspaces from the workspace store
    /// and uses the current process cwd unless overridden.
    pub fn resolve(&self, query: &str, override_cwd: Option<&str>) -> ResolveOutcome {
        let idx = self.ensure();
        let recent = self.recent_workspace_paths();
        let cwd = override_cwd.map(str::to_string).or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|p| p.to_string_lossy().into_owned())
        });
        let ctx = ResolveContext {
            recent_paths: &recent,
            current_cwd: cwd.as_deref(),
            top_k: 5,
        };
        resolver::resolve(query, &idx, &ctx)
    }

    fn recent_workspace_paths(&self) -> Vec<String> {
        self.ws_mgr
            .list()
            .map(|list| {
                let mut out = Vec::new();
                for w in list {
                    for p in w.paths {
                        out.push(p);
                    }
                }
                out
            })
            .unwrap_or_default()
    }

    pub fn list_projects(&self) -> Vec<ProjectEntry> {
        self.ensure().projects
    }

    /// Add an alias to a project's `.pigmemory/aliases.json`. Re-indexes
    /// the affected entry in place so later resolves pick it up without
    /// a full rebuild.
    pub fn add_alias(&self, path: &Path, alias: &str) -> Result<Vec<String>> {
        let canon = path
            .canonicalize()
            .map_err(|e| Error::NotFound(format!("{}: {}", path.display(), e)))?;
        let aliases = aliases::add(&canon, alias)?;
        self.refresh_one(&canon);
        Ok(aliases)
    }

    pub fn remove_alias(&self, path: &Path, alias: &str) -> Result<Vec<String>> {
        let canon = path
            .canonicalize()
            .map_err(|e| Error::NotFound(format!("{}: {}", path.display(), e)))?;
        let aliases = aliases::remove(&canon, alias)?;
        self.refresh_one(&canon);
        Ok(aliases)
    }

    fn refresh_one(&self, project: &Path) {
        let canon_str = project.to_string_lossy().to_string();
        let Some(mut idx) = self.index.read().clone() else {
            return;
        };
        let new_aliases = aliases::load(project);
        let mut hit = false;
        for entry in idx.projects.iter_mut() {
            if entry.path == canon_str {
                entry.aliases = new_aliases.clone();
                hit = true;
                break;
            }
        }
        if !hit && crate::project_resolver::parsers::is_project_root(project) {
            let signals = crate::project_resolver::parsers::parse_all(project);
            idx.projects.push(ProjectEntry {
                path: canon_str,
                dirname: crate::project_resolver::parsers::dirname(project),
                kinds: signals.kinds,
                names: signals.names,
                descriptions: signals.descriptions,
                headings: signals.headings,
                remote: signals.remote,
                languages: signals.languages,
                aliases: new_aliases,
                mtime: 0,
            });
        }
        let _ = indexer::save_cache(&idx);
        *self.index.write() = Some(idx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fresh_db() -> DbPool {
        // In-memory pool isolates the test from the user's real DB.
        let mgr = r2d2_sqlite::SqliteConnectionManager::memory();
        let pool: DbPool = r2d2::Pool::builder().max_size(1).build(mgr).unwrap();
        // Settings table is the only one we touch.
        pool.get()
            .unwrap()
            .execute_batch(
                "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 CREATE TABLE workspaces (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    layout_json TEXT NOT NULL DEFAULT '{\"type\":\"empty\"}',
                    paths_json TEXT NOT NULL DEFAULT '[]'
                 );
                 CREATE TABLE agents (
                    id TEXT PRIMARY KEY,
                    workspace_id TEXT NOT NULL,
                    type TEXT NOT NULL,
                    cwd TEXT,
                    status TEXT NOT NULL DEFAULT 'exited',
                    created_at TEXT NOT NULL
                 );",
            )
            .unwrap();
        pool
    }

    #[test]
    fn add_alias_then_resolve_via_alias() {
        let db = fresh_db();
        let ws = Arc::new(WorkspaceManager::new(db.clone()));

        let dir =
            std::env::temp_dir().join(format!("pigide-resolver-svc-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        // One real project on disk.
        let proj = dir.join("drugs-tracker-plugin");
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join("Cargo.toml"), "[package]\nname=\"x\"").unwrap();

        // Point the resolver at our temp root.
        let roots_json = serde_json::to_string(&vec![dir.to_string_lossy()]).unwrap();
        crate::db::set_setting(&db, "project_resolver.roots", &roots_json).unwrap();

        let svc = ResolverService::new(db.clone(), ws.clone());
        // Pin the index to *only* our temp root so the test isn't perturbed
        // by the developer's real `$HOME/code/...` projects.
        svc.rebuild_with_options(crate::project_resolver::ScanOptions {
            roots: vec![dir.clone()],
            max_depth: 4,
            home_max_depth: 2,
        });
        let r = svc.resolve("наркотики", None);
        // Without an alias the cyrillic query won't find it.
        assert!(matches!(
            r.status,
            crate::project_resolver::ResolveStatus::NotFound
        ));

        svc.add_alias(&proj, "наркотики").unwrap();
        let r2 = svc.resolve("наркотики", None);
        assert!(matches!(
            r2.status,
            crate::project_resolver::ResolveStatus::Found
        ));

        fs::remove_dir_all(&dir).ok();
    }
}
