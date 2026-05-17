//! Resolve the per-workspace `.pigmemory/` root.

use crate::error::{Error, Result};
use crate::workspace::WorkspaceManager;
use std::path::PathBuf;

const MEMORY_DIR: &str = ".pigmemory";

/// Resolve the memory root for the given workspace.
///
/// Strategy:
/// 1. If `workspace.paths[0]` is set, return `<paths[0]>/.pigmemory/`.
/// 2. Else fall back to `~/.config/pigide/memory/<workspace_id>/`.
///
/// The directory is created lazily; callers should `ensure_root` before
/// reading or writing.
pub fn resolve_root(ws_mgr: &WorkspaceManager, workspace_id: &str) -> Result<PathBuf> {
    let ws = ws_mgr.get(workspace_id)?;
    if let Some(p) = ws.paths.iter().find(|p| !p.is_empty()) {
        return Ok(PathBuf::from(p).join(MEMORY_DIR));
    }
    // Fallback under XDG config.
    let base =
        dirs::config_dir().ok_or_else(|| Error::Other("config dir unavailable".into()))?;
    Ok(base.join("pigide").join("memory").join(workspace_id))
}

pub fn ensure_root(root: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(root)?;
    Ok(())
}

/// Convert a slug into an absolute file path under `root`.
pub fn slug_to_path(root: &std::path::Path, slug: &str) -> PathBuf {
    let mut p = root.to_path_buf();
    for seg in slug.split('/') {
        p.push(seg);
    }
    p.set_extension("md");
    p
}

/// Recover slug from absolute path, given the root.
pub fn path_to_slug(root: &std::path::Path, path: &std::path::Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let rel_no_ext = rel.with_extension("");
    let s = rel_no_ext.to_string_lossy().replace('\\', "/");
    if s.is_empty() { None } else { Some(s) }
}

/// Make a kebab-case slug from a free-form title.
pub fn slugify(title: &str) -> String {
    slug::slugify(title)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn slug_round_trip() {
        let root = Path::new("/tmp/x/.pigmemory");
        let p = slug_to_path(root, "decisions/auth-pattern");
        assert_eq!(p, Path::new("/tmp/x/.pigmemory/decisions/auth-pattern.md"));
        let back = path_to_slug(root, &p).unwrap();
        assert_eq!(back, "decisions/auth-pattern");
    }

    #[test]
    fn slugify_strips_punctuation() {
        assert_eq!(slugify("Auth Pattern!"), "auth-pattern");
    }
}
