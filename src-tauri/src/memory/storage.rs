//! Resolve the per-workspace `.pigmemory/` root.

use crate::error::{Error, Result};
use crate::workspace::WorkspaceManager;
use std::path::{Path, PathBuf};

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
    let base = dirs::config_dir().ok_or_else(|| Error::Other("config dir unavailable".into()))?;
    Ok(base.join("pigide").join("memory").join(workspace_id))
}

pub fn ensure_root(root: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(root)?;
    Ok(())
}

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
    Ok(p)
}

fn validate_slug(slug: &str) -> Result<()> {
    if slug.is_empty()
        || slug == "."
        || slug.contains("..")
        || slug.contains('/')
        || slug.contains('\\')
        || slug.contains('\0')
    {
        return Err(Error::Invalid(format!("invalid memory slug: {}", slug)));
    }
    Ok(())
}

/// Recover slug from absolute path, given the root.
pub fn path_to_slug(root: &std::path::Path, path: &std::path::Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let rel_no_ext = rel.with_extension("");
    let s = rel_no_ext.to_string_lossy().replace('\\', "/");
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Make a kebab-case slug from a free-form title.
pub fn slugify(title: &str) -> String {
    slug::slugify(title)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slug_round_trip() {
        let root = tempdir_for_test("pigide-memory-roundtrip");
        let p = slug_to_path(&root, "auth-pattern").unwrap();
        assert_eq!(p, root.join("auth-pattern.md"));
        let back = path_to_slug(&root, &p).unwrap();
        assert_eq!(back, "auth-pattern");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn slug_rejects_traversal() {
        let root = tempdir_for_test("pigide-memory-traversal");
        let err = slug_to_path(&root, "../../etc/cron.d/backdoor").unwrap_err();
        assert!(err.to_string().contains("invalid memory slug"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn slug_rejects_path_separators_and_dot_segments() {
        let root = tempdir_for_test("pigide-memory-separators");
        assert!(slug_to_path(&root, "nested/hidden").is_err());
        assert!(slug_to_path(&root, r"nested\hidden").is_err());
        assert!(slug_to_path(&root, ".").is_err());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn slugify_strips_punctuation() {
        assert_eq!(slugify("Auth Pattern!"), "auth-pattern");
    }

    fn tempdir_for_test(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("{}-{}", label, uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
