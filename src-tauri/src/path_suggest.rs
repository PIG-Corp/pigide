//! `@`-mention path suggestion + validation for Architect chat attachments.
//!
//! Two responsibilities:
//!   * `suggest()` — given a free-form query (`@/abs/path`, `@./rel`,
//!     `@rel/sub`, `@basename`), return a ranked, capped list of files and
//!     directories that match. Bounded traversal — never blocks the IPC
//!     thread for long.
//!   * `validate()` — given a user-supplied path string, canonicalise it and
//!     enforce the allow-list (active workspace root, all workspace paths,
//!     and the user's home directory). Symlink-escape and `..` traversal
//!     attacks are blocked by canonicalisation + prefix check.
//!
//! Scope: this module only knows about paths. The orchestrator's `[WORLD
//! STATE]` rendering and the chat-queue plumbing live elsewhere; both
//! consume `Attachment`.
//!
//! Inspired by the existing `files::walk_files` helper and the resolver's
//! `fuzzy::fuzzy_score`.

use crate::db::DbPool;
use crate::error::{Error, Result};
use crate::project_resolver::fuzzy;
use crate::workspace::WorkspaceManager;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Hard cap on how many entries `suggest()` returns to the UI.
pub const MAX_SUGGESTIONS: usize = 20;

/// Hard cap on how many filesystem entries `walk()` will visit per call —
/// keeps a 50k-file repo from blowing the IPC thread.
pub const MAX_WALK_ENTRIES: usize = 8000;

/// Per-message attachment cap. Anything past this is rejected up front so
/// the orchestrator's WORLD STATE block stays bounded.
pub const MAX_ATTACHMENTS_PER_MESSAGE: usize = 16;

/// Maximum length of a single attachment path. Defends against pathological
/// payloads.
pub const MAX_PATH_LEN: usize = 4096;

/// Directory names skipped during recursive walks. Mirrors `files::walk_files`
/// plus a few build-cache aliases.
const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    ".pnpm-store",
    ".pigmemory",
    ".next",
    ".turbo",
    ".cache",
    ".venv",
    "__pycache__",
];

/// Single suggestion row returned to the UI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Suggestion {
    /// Absolute, canonicalised path.
    pub path: String,
    /// Workspace-relative label when the path lives inside the active
    /// workspace; otherwise the absolute path with `~` collapsed.
    pub label: String,
    /// `"file"` or `"dir"`.
    pub kind: String,
}

/// Validated, server-side-resolved attachment that travels alongside a
/// user message into the orchestrator. The frontend's pre-validation copy
/// of this lives in `frontend/src/state/types.ts` (kept in sync by hand).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Attachment {
    /// `"file"` or `"dir"`. Validation rejects anything else.
    pub kind: String,
    /// Absolute, canonicalised path inside the allow-list.
    pub path: String,
    /// Display label (workspace-relative or `~`-collapsed absolute).
    pub label: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SuggestArgs {
    pub query: String,
    /// Workspace id to scope relative-path suggestions to. When `None`,
    /// only absolute paths and `$HOME`-relative entries are suggested.
    #[serde(default)]
    pub workspace_id: Option<String>,
}

/// Trim a leading `@` if the caller forwarded the trigger character along
/// with the query. Both forms must work — the frontend strips it but the
/// MCP/CLI path may not.
fn strip_trigger(q: &str) -> &str {
    q.strip_prefix('@').unwrap_or(q)
}

/// Resolve the workspace root for `workspace_id`, falling back to the
/// active workspace stored under `current_workspace_id`. Returns the first
/// non-empty entry of the workspace's `paths`.
fn workspace_root(
    db: &DbPool,
    ws_mgr: &WorkspaceManager,
    workspace_id: Option<&str>,
) -> Option<PathBuf> {
    let id_owned: Option<String> = match workspace_id {
        Some(id) if !id.is_empty() => Some(id.to_string()),
        _ => crate::db::get_setting(db, "current_workspace_id")
            .ok()
            .flatten()
            .filter(|s| !s.is_empty()),
    };
    let id = id_owned?;
    let ws = ws_mgr.get(&id).ok()?;
    ws.paths
        .into_iter()
        .find(|p| !p.is_empty())
        .map(PathBuf::from)
}

/// Build the allow-list: every workspace's `paths[*]` plus the user's
/// home directory (canonicalised). Falsey/missing entries are skipped.
/// Used by `validate()` for the "no escaping the sandbox" check.
pub fn allow_roots(ws_mgr: &WorkspaceManager) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    if let Some(home) = dirs::home_dir() {
        if let Ok(c) = home.canonicalize() {
            out.push(c);
        }
    }
    if let Ok(list) = ws_mgr.list() {
        for ws in list {
            for p in ws.paths {
                if p.is_empty() {
                    continue;
                }
                let pb = PathBuf::from(&p);
                if let Ok(c) = pb.canonicalize() {
                    if !out.iter().any(|r| r == &c) {
                        out.push(c);
                    }
                }
            }
        }
    }
    out
}

/// Collapse `~` for display when `path` is inside the user's home dir.
fn collapse_home(path: &Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(stripped) = path.strip_prefix(&home) {
            let s = stripped.to_string_lossy();
            return if s.is_empty() {
                "~".to_string()
            } else {
                format!("~/{}", s)
            };
        }
    }
    path.to_string_lossy().into_owned()
}

/// Compute the display label for a path: workspace-relative when inside
/// the active workspace, `~`-collapsed otherwise.
pub fn display_label(path: &Path, workspace_root: Option<&Path>) -> String {
    if let Some(root) = workspace_root {
        if let Ok(rel) = path.strip_prefix(root) {
            let s = rel.to_string_lossy();
            return if s.is_empty() {
                ".".to_string()
            } else {
                s.into_owned()
            };
        }
    }
    collapse_home(path)
}

/// `(kind, label, suggestion-path)` triple from a fully resolved entry.
fn to_suggestion(path: &Path, is_dir: bool, workspace_root: Option<&Path>) -> Suggestion {
    let kind = if is_dir { "dir" } else { "file" };
    let label = display_label(path, workspace_root);
    let label = if is_dir && !label.ends_with('/') {
        format!("{}/", label)
    } else {
        label
    };
    Suggestion {
        path: path.to_string_lossy().into_owned(),
        label,
        kind: kind.to_string(),
    }
}

/// Suggest entries for an absolute-path query (`@/abs/path/frag`). Walks
/// the parent directory only — no recursion. The fragment after the last
/// `/` filters basenames case-insensitively.
fn suggest_absolute(query: &str, workspace_root: Option<&Path>) -> Vec<Suggestion> {
    let p = Path::new(query);
    // Decide which directory to list: if `query` ends with a `/`, list
    // the directory itself; otherwise list its parent and filter on the
    // remaining basename fragment.
    let (dir, frag) = if query.ends_with('/') {
        (p.to_path_buf(), String::new())
    } else {
        let parent = p
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("/"));
        let frag = p
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        (parent, frag)
    };
    let frag_lower = frag.to_lowercase();
    let mut out: Vec<Suggestion> = Vec::new();
    let read = match std::fs::read_dir(&dir) {
        Ok(r) => r,
        Err(_) => return out,
    };
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !frag.is_empty() && !name.to_lowercase().contains(&frag_lower) {
            continue;
        }
        let path = entry.path();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        out.push(to_suggestion(&path, is_dir, workspace_root));
        if out.len() >= MAX_SUGGESTIONS * 2 {
            break;
        }
    }
    rank_and_cap(&mut out, &frag_lower);
    out
}

/// Order suggestions by:
///   1. Exact basename match (case-insensitive)
///   2. Basename starts with the fragment
///   3. Fuzzy score (descending)
///   4. Shorter labels first (ties)
fn rank_and_cap(out: &mut Vec<Suggestion>, frag_lower: &str) {
    if frag_lower.is_empty() {
        // No query fragment — alpha-sort directories first, then files.
        out.sort_by(|a, b| match (a.kind.as_str(), b.kind.as_str()) {
            ("dir", "file") => std::cmp::Ordering::Less,
            ("file", "dir") => std::cmp::Ordering::Greater,
            _ => a.label.to_lowercase().cmp(&b.label.to_lowercase()),
        });
        out.truncate(MAX_SUGGESTIONS);
        return;
    }
    out.sort_by(|a, b| {
        let an = basename(&a.path).to_lowercase();
        let bn = basename(&b.path).to_lowercase();
        let an_eq = an == frag_lower;
        let bn_eq = bn == frag_lower;
        if an_eq != bn_eq {
            return bn_eq.cmp(&an_eq); // exact first
        }
        let an_pref = an.starts_with(frag_lower);
        let bn_pref = bn.starts_with(frag_lower);
        if an_pref != bn_pref {
            return bn_pref.cmp(&an_pref);
        }
        // Fuzzy descending.
        let af = fuzzy::fuzzy_score(frag_lower, &an);
        let bf = fuzzy::fuzzy_score(frag_lower, &bn);
        bf.partial_cmp(&af)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.label.len().cmp(&b.label.len()))
    });
    out.truncate(MAX_SUGGESTIONS);
}

fn basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// Bounded recursive walk — visits up to `MAX_WALK_ENTRIES` entries.
fn walk(root: &Path, frag_lower: &str, workspace_root: Option<&Path>) -> Vec<Suggestion> {
    let mut out: Vec<Suggestion> = Vec::new();
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    let mut visited: usize = 0;
    while let Some(dir) = stack.pop() {
        if visited >= MAX_WALK_ENTRIES {
            break;
        }
        let read = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for entry in read.flatten() {
            visited += 1;
            if visited >= MAX_WALK_ENTRIES {
                break;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            // Hide most dotfiles by default; let absolute-path mode reach
            // them if the user really wants.
            if name.starts_with('.') && name != ".pigmemory" {
                continue;
            }
            if SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            let path = entry.path();
            let ft = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            let is_dir = ft.is_dir();
            // Match basename or workspace-relative path.
            let matches = if frag_lower.is_empty() {
                false
            } else {
                let basename_l = name.to_lowercase();
                if basename_l.contains(frag_lower) {
                    true
                } else {
                    let label = display_label(&path, workspace_root).to_lowercase();
                    label.contains(frag_lower)
                }
            };
            if matches {
                out.push(to_suggestion(&path, is_dir, workspace_root));
                if out.len() >= MAX_SUGGESTIONS * 4 {
                    // Plenty of candidates already, stop walking.
                    return out;
                }
            }
            if is_dir {
                stack.push(path);
            }
        }
    }
    out
}

/// Workspace-scoped suggestion entry point. `query` may already include
/// the leading `@`; absolute and `./` and `~/` forms are all accepted.
pub fn suggest(
    db: &DbPool,
    ws_mgr: &WorkspaceManager,
    args: SuggestArgs,
) -> Result<Vec<Suggestion>> {
    let raw = strip_trigger(args.query.trim());
    let ws_root = workspace_root(db, ws_mgr, args.workspace_id.as_deref());

    // ~/abs path
    let expanded: PathBuf = if let Some(stripped) = raw.strip_prefix("~/") {
        match dirs::home_dir() {
            Some(h) => h.join(stripped),
            None => return Ok(Vec::new()),
        }
    } else if raw == "~" {
        match dirs::home_dir() {
            Some(h) => h,
            None => return Ok(Vec::new()),
        }
    } else if raw.starts_with('/') {
        PathBuf::from(raw)
    } else if let Some(stripped) = raw.strip_prefix("./") {
        match &ws_root {
            Some(r) => r.join(stripped),
            None => return Ok(Vec::new()),
        }
    } else {
        // Workspace-scoped: bare basename or relative path.
        let mut suggestions = Vec::new();
        if let Some(root) = &ws_root {
            // Treat as relative if it contains `/`, else fuzzy basename.
            if raw.contains('/') {
                let target = root.join(raw);
                let absolute_query = target.to_string_lossy().into_owned();
                suggestions.extend(suggest_absolute(&absolute_query, Some(root)));
            } else {
                let frag_lower = raw.to_lowercase();
                suggestions = walk(root, &frag_lower, Some(root));
                rank_and_cap(&mut suggestions, &frag_lower);
            }
        }
        return Ok(suggestions);
    };
    // Absolute / `~`-expanded / `./`-expanded path.
    let absolute = expanded.to_string_lossy().into_owned();
    Ok(suggest_absolute(&absolute, ws_root.as_deref()))
}

/// Reject obvious-bad input shapes before touching the filesystem.
fn validate_shape(path: &str) -> Result<()> {
    if path.is_empty() {
        return Err(Error::Invalid("empty path".into()));
    }
    if path.len() > MAX_PATH_LEN {
        return Err(Error::Invalid(format!(
            "path too long ({}; max {})",
            path.len(),
            MAX_PATH_LEN
        )));
    }
    // Disallow embedded NULs that some kernels would silently truncate on.
    if path.contains('\0') {
        return Err(Error::Invalid("path contains NUL byte".into()));
    }
    Ok(())
}

/// Verify that `path` resolves to a real entry inside one of the allow-list
/// roots. Symlink-escape is blocked by canonicalisation: we canonicalise
/// both the candidate and every root, then ensure the candidate is a prefix
/// match of at least one root.
pub fn validate(ws_mgr: &WorkspaceManager, raw_path: &str) -> Result<Attachment> {
    validate_shape(raw_path)?;
    let path = PathBuf::from(raw_path);
    let canon = path
        .canonicalize()
        .map_err(|e| Error::NotFound(format!("path {:?} ({})", path, e)))?;
    let meta = std::fs::metadata(&canon)
        .map_err(|e| Error::NotFound(format!("metadata {:?} ({})", canon, e)))?;
    let kind = if meta.is_dir() { "dir" } else { "file" };

    let roots = allow_roots(ws_mgr);
    if roots.is_empty() {
        return Err(Error::Invalid(
            "no allow-list roots configured (no workspaces, no home dir)".into(),
        ));
    }
    let inside = roots.iter().any(|root| canon.starts_with(root));
    if !inside {
        return Err(Error::Invalid(format!(
            "path is outside the allow-list: {}",
            canon.display()
        )));
    }
    // Pick the most-specific root (longest prefix) for the display label —
    // workspace root beats `$HOME` when both apply.
    let root = roots
        .iter()
        .filter(|r| canon.starts_with(r))
        .max_by_key(|r| r.as_os_str().len())
        .cloned();
    // Distinguish "active workspace root" (used for label) from `$HOME`.
    let workspace_root = root.filter(|r| dirs::home_dir().map(|h| r != &h).unwrap_or(true));
    let label = display_label(&canon, workspace_root.as_deref());
    Ok(Attachment {
        kind: kind.to_string(),
        path: canon.to_string_lossy().into_owned(),
        label,
    })
}

/// Validate the full attachment list a user submitted. Enforces the
/// per-message cap and de-duplicates by canonical path. Returns
/// `Err(...)` on the first invalid entry — surfacing a clear,
/// path-specific error to the UI.
pub fn validate_all(ws_mgr: &WorkspaceManager, raw: &[Attachment]) -> Result<Vec<Attachment>> {
    if raw.len() > MAX_ATTACHMENTS_PER_MESSAGE {
        return Err(Error::Invalid(format!(
            "too many attachments ({}; max {})",
            raw.len(),
            MAX_ATTACHMENTS_PER_MESSAGE
        )));
    }
    let mut out: Vec<Attachment> = Vec::with_capacity(raw.len());
    for a in raw {
        let v = validate(ws_mgr, &a.path)?;
        if !out.iter().any(|x| x.path == v.path) {
            out.push(v);
        }
    }
    Ok(out)
}

/// Render the validated attachment list into the orchestrator's
/// `[WORLD STATE]` block. Empty list → empty string (so we don't leak a
/// trailing section header).
pub fn render_world_state(attachments: &[Attachment]) -> String {
    if attachments.is_empty() {
        return String::new();
    }
    let mut s = String::from("attachments (current turn — user-pinned context):\n");
    for a in attachments {
        s.push_str(&format!(
            "  - kind={} label={:?} path={}\n",
            a.kind, a.label, a.path
        ));
    }
    s.push_str(
        "  (these are paths the user explicitly attached. \
         Read them with file tools if relevant; do not assume contents.)\n",
    );
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Arc;

    fn fresh_db_with_ws(paths: Vec<&str>) -> (DbPool, Arc<WorkspaceManager>, String) {
        let mgr = r2d2_sqlite::SqliteConnectionManager::memory();
        let pool: DbPool = r2d2::Pool::builder().max_size(1).build(mgr).unwrap();
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
        let ws = Arc::new(WorkspaceManager::new(pool.clone()));
        let path_strs: Vec<String> = paths.iter().map(|s| s.to_string()).collect();
        let w = ws.create("test", path_strs).unwrap();
        crate::db::set_setting(&pool, "current_workspace_id", &w.id).unwrap();
        (pool, ws, w.id)
    }

    fn tmp_dir(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "pigide-path-suggest-{}-{}",
            name,
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&p).unwrap();
        p.canonicalize().unwrap()
    }

    #[test]
    fn suggest_absolute_path_lists_directory() {
        let dir = tmp_dir("abs");
        fs::write(dir.join("alpha.rs"), "// hi").unwrap();
        fs::write(dir.join("beta.rs"), "// hi").unwrap();
        fs::create_dir_all(dir.join("subdir")).unwrap();

        let (db, ws, _) = fresh_db_with_ws(vec![dir.to_str().unwrap()]);
        let q = format!("@{}/al", dir.display());
        let out = suggest(
            &db,
            &ws,
            SuggestArgs {
                query: q,
                workspace_id: None,
            },
        )
        .unwrap();
        assert!(
            out.iter().any(|s| s.path.ends_with("alpha.rs")),
            "out={:?}",
            out
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn suggest_workspace_relative_walks_root() {
        let dir = tmp_dir("ws");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(dir.join("src/lib.rs"), "pub fn x() {}").unwrap();

        let (db, ws, _) = fresh_db_with_ws(vec![dir.to_str().unwrap()]);
        let out = suggest(
            &db,
            &ws,
            SuggestArgs {
                query: "@main".to_string(),
                workspace_id: None,
            },
        )
        .unwrap();
        assert!(
            out.iter().any(|s| s.path.ends_with("main.rs")),
            "out={:?}",
            out
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn suggest_skips_ignored_dirs() {
        let dir = tmp_dir("ignored");
        // Ignored: target/build.rs should NOT come back from a bare-fragment
        // search.
        fs::create_dir_all(dir.join("target")).unwrap();
        fs::write(dir.join("target/build.rs"), "// build").unwrap();
        // Normal: src/build.rs is fine.
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/build.rs"), "// real").unwrap();

        let (db, ws, _) = fresh_db_with_ws(vec![dir.to_str().unwrap()]);
        let out = suggest(
            &db,
            &ws,
            SuggestArgs {
                query: "@build".to_string(),
                workspace_id: None,
            },
        )
        .unwrap();
        assert!(
            out.iter().any(|s| s.path.ends_with("src/build.rs")),
            "missing src/build.rs: {:?}",
            out
        );
        assert!(
            !out.iter().any(|s| s.path.ends_with("target/build.rs")),
            "target/build.rs should be skipped: {:?}",
            out
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn validate_accepts_path_inside_workspace() {
        let dir = tmp_dir("validate-ok");
        let target = dir.join("hello.txt");
        fs::write(&target, "hi").unwrap();
        let (_db, ws, _) = fresh_db_with_ws(vec![dir.to_str().unwrap()]);
        let a = validate(&ws, target.to_str().unwrap()).unwrap();
        assert_eq!(a.kind, "file");
        // Label should be workspace-relative, not absolute.
        assert_eq!(a.label, "hello.txt");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn validate_rejects_path_outside_allow_list() {
        // Allow-list = a temp workspace; we then try to attach `/etc/hosts`
        // which is outside both the workspace and `$HOME`.
        let dir = tmp_dir("validate-out");
        let (_db, ws, _) = fresh_db_with_ws(vec![dir.to_str().unwrap()]);
        // Sanity: /etc/hosts exists on linux test runners but is not under
        // the workspace nor $HOME (assuming test runner isn't `root` with
        // $HOME=/etc, which would be deeply broken).
        let r = validate(&ws, "/etc/hosts");
        match r {
            Ok(_) => {
                // If $HOME happens to contain /etc on the runner this
                // assertion is moot — skip rather than mis-fail.
                let home = dirs::home_dir().unwrap_or_default();
                assert!(
                    PathBuf::from("/etc/hosts")
                        .canonicalize()
                        .map(|c| c.starts_with(&home))
                        .unwrap_or(false),
                    "validate accepted /etc/hosts but it isn't inside $HOME"
                );
            }
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("outside the allow-list") || msg.contains("not found"),
                    "unexpected error: {}",
                    msg
                );
            }
        }
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn validate_rejects_traversal_via_dotdot() {
        // `<workspace>/../<file_outside>` must canonicalise OUT of the
        // workspace and trip the allow-list check.
        let parent = tmp_dir("validate-traverse");
        let workspace = parent.join("ws");
        fs::create_dir_all(&workspace).unwrap();
        let outside_file = parent.join("secret.txt");
        fs::write(&outside_file, "secret").unwrap();

        let (_db, ws, _) = fresh_db_with_ws(vec![workspace.to_str().unwrap()]);
        // Construct a non-canonical path that escapes via `..`.
        let traversal = format!("{}/../secret.txt", workspace.display());
        let r = validate(&ws, &traversal);
        if r.is_ok() {
            // Acceptable only if `secret.txt` happens to live inside `$HOME`.
            let home = dirs::home_dir().unwrap_or_default();
            assert!(outside_file.canonicalize().unwrap().starts_with(&home));
        }
        fs::remove_dir_all(&parent).ok();
    }

    #[test]
    fn validate_rejects_symlink_escape() {
        // Create a symlink INSIDE the workspace that points to a file
        // OUTSIDE both the workspace and $HOME. canonicalize() must follow
        // the link and the prefix check must then reject it.
        let parent = tmp_dir("validate-symlink");
        let workspace = parent.join("ws");
        fs::create_dir_all(&workspace).unwrap();
        // Outside file. We deliberately stash it in a sibling dir of the
        // workspace, NOT under $HOME. Skip on platforms / runners where
        // that sibling happens to be inside $HOME.
        let outside = parent.join("escape.txt");
        fs::write(&outside, "owned").unwrap();
        let outside_canon = outside.canonicalize().unwrap();
        if let Some(home) = dirs::home_dir() {
            if outside_canon.starts_with(&home) {
                fs::remove_dir_all(&parent).ok();
                return;
            }
        }
        let link = workspace.join("ln");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        #[cfg(not(unix))]
        {
            // Skip — we don't ship Windows builds for this surface yet.
            fs::remove_dir_all(&parent).ok();
            return;
        }

        let (_db, ws, _) = fresh_db_with_ws(vec![workspace.to_str().unwrap()]);
        let r = validate(&ws, link.to_str().unwrap());
        assert!(
            r.is_err(),
            "expected symlink escape to be rejected, got {:?}",
            r
        );
        fs::remove_dir_all(&parent).ok();
    }

    #[test]
    fn validate_rejects_nonexistent_path() {
        let dir = tmp_dir("validate-missing");
        let (_db, ws, _) = fresh_db_with_ws(vec![dir.to_str().unwrap()]);
        let phantom = dir.join("nope.txt");
        let r = validate(&ws, phantom.to_str().unwrap());
        assert!(r.is_err());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn validate_all_enforces_attachment_cap() {
        let dir = tmp_dir("validate-cap");
        let (_db, ws, _) = fresh_db_with_ws(vec![dir.to_str().unwrap()]);
        let mut raw = Vec::new();
        for i in 0..(MAX_ATTACHMENTS_PER_MESSAGE + 1) {
            let p = dir.join(format!("f{}.txt", i));
            fs::write(&p, "x").unwrap();
            raw.push(Attachment {
                kind: "file".into(),
                path: p.to_string_lossy().into_owned(),
                label: format!("f{}.txt", i),
            });
        }
        let r = validate_all(&ws, &raw);
        assert!(r.is_err());
        let err = r.err().unwrap().to_string();
        assert!(err.contains("too many attachments"), "err={}", err);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn validate_all_dedups_same_path() {
        let dir = tmp_dir("validate-dedup");
        let f = dir.join("dup.txt");
        fs::write(&f, "x").unwrap();
        let (_db, ws, _) = fresh_db_with_ws(vec![dir.to_str().unwrap()]);
        let raw = vec![
            Attachment {
                kind: "file".into(),
                path: f.to_string_lossy().into_owned(),
                label: "dup.txt".into(),
            },
            Attachment {
                kind: "file".into(),
                path: f.to_string_lossy().into_owned(),
                label: "dup.txt".into(),
            },
        ];
        let v = validate_all(&ws, &raw).unwrap();
        assert_eq!(v.len(), 1);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn render_world_state_renders_each_entry() {
        let xs = vec![
            Attachment {
                kind: "file".into(),
                path: "/abs/path.rs".into(),
                label: "src/path.rs".into(),
            },
            Attachment {
                kind: "dir".into(),
                path: "/abs/dir".into(),
                label: "dir/".into(),
            },
        ];
        let s = render_world_state(&xs);
        assert!(s.contains("attachments"));
        assert!(s.contains("src/path.rs"));
        assert!(s.contains("dir/"));
        assert!(s.contains("/abs/path.rs"));
    }

    #[test]
    fn render_world_state_empty_returns_empty_string() {
        assert_eq!(render_world_state(&[]), "");
    }

    #[test]
    fn validate_rejects_nul_byte_and_long_paths() {
        let dir = tmp_dir("validate-shape");
        let (_db, ws, _) = fresh_db_with_ws(vec![dir.to_str().unwrap()]);
        assert!(validate(&ws, "").is_err());
        assert!(validate(&ws, "abc\0def").is_err());
        let too_long = "a".repeat(MAX_PATH_LEN + 1);
        assert!(validate(&ws, &too_long).is_err());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn display_label_collapses_home_when_outside_workspace() {
        // Simulate a path inside $HOME but outside the workspace — label
        // should fall back to `~/...`.
        let home = match dirs::home_dir() {
            Some(h) => h,
            None => return,
        };
        let inside_home = home.join("some-file.txt");
        let label = display_label(&inside_home, None);
        assert!(label.starts_with("~/"), "label={}", label);
    }
}
