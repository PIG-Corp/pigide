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
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(p)
}

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
            root.join("chats")
                .join("claude-tile-1")
                .join("2026-05-27.md")
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
