//! Bounded-depth recursive scan of project roots.

use crate::project_resolver::aliases;
use crate::project_resolver::parsers;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// One indexed project as written to the cache.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectEntry {
    pub path: String,
    pub dirname: String,
    pub kinds: Vec<String>,
    pub names: Vec<String>,
    pub descriptions: Vec<String>,
    pub headings: Vec<String>,
    pub remote: Option<String>,
    pub languages: Vec<String>,
    pub aliases: Vec<String>,
    pub mtime: i64,
}

/// In-memory index. Cheap to clone and serialize.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectIndex {
    pub version: u32,
    pub built_at: String,
    pub roots: Vec<String>,
    pub projects: Vec<ProjectEntry>,
}

const INDEX_VERSION: u32 = 1;
const DEFAULT_MAX_DEPTH: usize = 5;
const HOME_MAX_DEPTH: usize = 2;

/// Directory names we never descend into.
const EXCLUDE_DIR_NAMES: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    ".git",
    ".next",
    ".nuxt",
    "venv",
    ".venv",
    "__pycache__",
    ".pnpm-store",
    ".cache",
    ".gradle",
    ".idea",
    ".vscode",
    ".ssh",
    ".gnupg",
    ".cargo",
    ".rustup",
    ".npm",
    ".pnpm",
    ".yarn",
    ".docker",
];

/// Absolute paths we never index. Real-world only: tests run inside
/// `$TMPDIR` so we don't include `/tmp` here. Production users with
/// projects under `/tmp` are an edge case we accept.
fn excluded_abs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    for p in &["/proc", "/sys", "/dev", "/run"] {
        v.push(PathBuf::from(p));
    }
    if let Some(home) = dirs::home_dir() {
        for child in &[
            ".cache", ".config", ".local", ".ssh", ".gnupg", ".cargo", ".rustup", ".npm", ".pnpm",
            ".yarn",
        ] {
            v.push(home.join(child));
        }
    }
    v
}

/// Resolve roots — `extra` first (highest priority), then defaults that exist
/// on disk. Each root is canonicalized and de-duplicated.
pub fn default_roots(extra: &[PathBuf]) -> Vec<PathBuf> {
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut out = Vec::new();
    let push = |p: PathBuf, out: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>| {
        if !p.is_dir() {
            return;
        }
        let canon = p.canonicalize().unwrap_or(p);
        if seen.insert(canon.clone()) {
            out.push(canon);
        }
    };
    for p in extra {
        push(p.clone(), &mut out, &mut seen);
    }
    if let Some(home) = dirs::home_dir() {
        for child in &["code", "projects", "dev", "src", "work", "Documents/code"] {
            push(home.join(child), &mut out, &mut seen);
        }
        // home itself with shallower depth — picks up `~/pigide` etc.
        push(home, &mut out, &mut seen);
    }
    out
}

fn excluded(path: &Path, abs_excludes: &[PathBuf]) -> bool {
    for ex in abs_excludes {
        if path.starts_with(ex) {
            return true;
        }
    }
    if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
        if EXCLUDE_DIR_NAMES.contains(&name) {
            return true;
        }
    }
    false
}

fn mtime_secs(path: &Path) -> i64 {
    let m = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return 0,
    };
    let mt = match m.modified() {
        Ok(t) => t,
        Err(_) => return 0,
    };
    mt.duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Walk one root with bounded depth, calling `visit` for every project root
/// found. Once a project root is visited we don't descend into it.
fn walk_root(
    root: &Path,
    max_depth: usize,
    abs_excludes: &[PathBuf],
    visit: &mut impl FnMut(&Path),
) {
    fn descend(
        path: &Path,
        depth: usize,
        max_depth: usize,
        abs_excludes: &[PathBuf],
        visit: &mut impl FnMut(&Path),
    ) {
        if !path.is_dir() {
            return;
        }
        if excluded(path, abs_excludes) {
            return;
        }
        // Don't follow symlinks — keeps us inside the root.
        if let Ok(meta) = std::fs::symlink_metadata(path) {
            if meta.file_type().is_symlink() {
                return;
            }
        }
        if parsers::is_project_root(path) {
            visit(path);
            return;
        }
        if depth >= max_depth {
            return;
        }
        let entries = match std::fs::read_dir(path) {
            Ok(e) => e,
            Err(_) => return,
        };
        for ent in entries.flatten() {
            let p = ent.path();
            // Skip non-dir fast.
            let ft = match ent.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            if ft.is_symlink() {
                continue;
            }
            if !ft.is_dir() {
                continue;
            }
            descend(&p, depth + 1, max_depth, abs_excludes, visit);
        }
    }
    descend(root, 0, max_depth, abs_excludes, visit);
}

pub struct ScanOptions {
    pub roots: Vec<PathBuf>,
    /// Default depth limit — applies to every root that isn't `$HOME`.
    pub max_depth: usize,
    /// Tighter limit for `$HOME` itself (so we don't crawl `~/Pictures`,
    /// `~/Downloads`, etc).
    pub home_max_depth: usize,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            roots: default_roots(&[]),
            max_depth: DEFAULT_MAX_DEPTH,
            home_max_depth: HOME_MAX_DEPTH,
        }
    }
}

/// Run a fresh scan and return the assembled `ProjectIndex`.
pub fn scan(opts: ScanOptions) -> ProjectIndex {
    let abs_excludes = excluded_abs();
    let home = dirs::home_dir();
    let mut found: HashSet<PathBuf> = HashSet::new();
    let mut entries: Vec<ProjectEntry> = Vec::new();

    for root in &opts.roots {
        let depth = if Some(root) == home.as_ref() {
            opts.home_max_depth
        } else {
            opts.max_depth
        };
        let mut visit = |project: &Path| {
            let canon = project
                .canonicalize()
                .unwrap_or_else(|_| project.to_path_buf());
            if !found.insert(canon.clone()) {
                return;
            }
            let signals = parsers::parse_all(&canon);
            let aliases = aliases::load(&canon);
            let dirname = parsers::dirname(&canon);
            let entry = ProjectEntry {
                path: canon.to_string_lossy().into_owned(),
                dirname,
                kinds: signals.kinds,
                names: signals.names,
                descriptions: signals.descriptions,
                headings: signals.headings,
                remote: signals.remote,
                languages: signals.languages,
                aliases,
                mtime: mtime_secs(&canon),
            };
            entries.push(entry);
        };
        walk_root(root, depth, &abs_excludes, &mut visit);
    }

    // Stable order: most-recently-modified first, then path.
    entries.sort_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| a.path.cmp(&b.path)));

    ProjectIndex {
        version: INDEX_VERSION,
        built_at: chrono::Utc::now().to_rfc3339(),
        roots: opts
            .roots
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect(),
        projects: entries,
    }
}

/// Index ↔ disk. Cache lives at `~/.cache/pigide/project-index.json`.
pub fn cache_path() -> Option<PathBuf> {
    let base = dirs::cache_dir()?;
    let dir = base.join("pigide");
    std::fs::create_dir_all(&dir).ok();
    Some(dir.join("project-index.json"))
}

pub fn load_cache() -> Option<ProjectIndex> {
    let path = cache_path()?;
    let raw = std::fs::read_to_string(&path).ok()?;
    let idx: ProjectIndex = serde_json::from_str(&raw).ok()?;
    if idx.version != INDEX_VERSION {
        return None;
    }
    Some(idx)
}

pub fn save_cache(idx: &ProjectIndex) -> std::io::Result<()> {
    let Some(path) = cache_path() else {
        return Ok(());
    };
    let raw = serde_json::to_string_pretty(idx).unwrap_or_default();
    std::fs::write(path, raw)
}

/// Cache TTL — a stale-but-readable index is still served while a
/// background refresh runs.
pub fn is_fresh(idx: &ProjectIndex, max_age: std::time::Duration) -> bool {
    let Ok(built) = chrono::DateTime::parse_from_rfc3339(&idx.built_at) else {
        return false;
    };
    let now = chrono::Utc::now().with_timezone(&built.timezone());
    let age = now.signed_duration_since(built);
    age.num_seconds() >= 0 && (age.num_seconds() as u64) <= max_age.as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    fn tmp(suffix: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "pigide-resolver-indexer-{}-{}",
            suffix,
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn make_project(root: &Path, name: &str, files: &[(&str, &str)]) -> PathBuf {
        let p = root.join(name);
        fs::create_dir_all(&p).unwrap();
        for (fname, body) in files {
            let mut f = fs::File::create(p.join(fname)).unwrap();
            f.write_all(body.as_bytes()).unwrap();
        }
        p
    }

    #[test]
    fn finds_projects_in_root() {
        let root = tmp("scan");
        let _a = make_project(
            &root,
            "alpha",
            &[("Cargo.toml", "[package]\nname=\"alpha\"")],
        );
        let _b = make_project(
            &root,
            "widget-plugin",
            &[(
                "paper-plugin.yml",
                "name: DrugsPlugin\ndescription: tracks\n",
            )],
        );
        // Nested inside an excluded dir → must not be indexed.
        let nm = root.join("alpha").join("node_modules").join("inner");
        fs::create_dir_all(&nm).unwrap();
        fs::write(nm.join("package.json"), r#"{"name":"inner"}"#).unwrap();

        let idx = scan(ScanOptions {
            roots: vec![root.clone()],
            max_depth: 4,
            home_max_depth: 2,
        });
        assert_eq!(idx.projects.len(), 2, "{:?}", idx.projects);
        let names: Vec<&str> = idx.projects.iter().map(|p| p.dirname.as_str()).collect();
        assert!(names.contains(&"alpha"));
        assert!(names.contains(&"widget-plugin"));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn does_not_descend_into_project() {
        let root = tmp("noinner");
        let outer = make_project(
            &root,
            "outer",
            &[("Cargo.toml", "[package]\nname=\"outer\"")],
        );
        // sub-package — should be ignored by the outer detection.
        let inner = outer.join("crates").join("inner");
        fs::create_dir_all(&inner).unwrap();
        fs::write(inner.join("Cargo.toml"), "[package]\nname=\"inner\"").unwrap();

        let idx = scan(ScanOptions {
            roots: vec![root.clone()],
            max_depth: 5,
            home_max_depth: 2,
        });
        assert_eq!(idx.projects.len(), 1);
        assert_eq!(idx.projects[0].dirname, "outer");
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn picks_up_aliases_from_pigmemory() {
        let root = tmp("alias");
        let proj = make_project(
            &root,
            "widget-plugin",
            &[("Cargo.toml", "[package]\nname=\"x\"")],
        );
        fs::create_dir_all(proj.join(".pigmemory")).unwrap();
        fs::write(
            proj.join(".pigmemory").join("aliases.json"),
            r#"{"aliases":["widget","widget plugin"]}"#,
        )
        .unwrap();
        let idx = scan(ScanOptions {
            roots: vec![root.clone()],
            max_depth: 5,
            home_max_depth: 2,
        });
        assert_eq!(idx.projects.len(), 1);
        assert!(idx.projects[0].aliases.iter().any(|a| a == "widget"));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn cold_scan_is_fast() {
        let root = tmp("perf");
        for i in 0..50 {
            make_project(
                &root,
                &format!("p{}", i),
                &[("Cargo.toml", "[package]\nname=\"p\"")],
            );
        }
        let started = std::time::Instant::now();
        let idx = scan(ScanOptions {
            roots: vec![root.clone()],
            max_depth: 4,
            home_max_depth: 2,
        });
        assert!(idx.projects.len() == 50);
        // 50 projects under one fresh root has to come in well under
        // the 5s budget for 200 projects.
        assert!(
            started.elapsed().as_secs() < 3,
            "took {:?}",
            started.elapsed()
        );
        fs::remove_dir_all(&root).ok();
    }
}
