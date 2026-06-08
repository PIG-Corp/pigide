//! Filesystem operations for workspace-backed file browsing and editing.
//!
//! All paths returned to callers are absolute; canonicalised through
//! `dunce::canonicalize` would be ideal but we keep it simpler with
//! `Path::canonicalize` — relying on the OS to resolve `..` and symlinks.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
}

pub fn list_dir(path: &str, allowed_roots: &[PathBuf]) -> Result<Vec<DirEntry>> {
    let p = validate_existing_workspace_path(path, allowed_roots)?;
    if !p.exists() {
        return Err(Error::NotFound(format!("path {:?}", p)));
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&p)? {
        let e = entry?;
        let file_type = e.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let meta = e.metadata()?;
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') && name != ".pigmemory" {
            // Hide most dotfiles by default; the orchestrator can still
            // request them by absolute path.
            continue;
        }
        out.push(DirEntry {
            name,
            path: e.path().to_string_lossy().to_string(),
            is_dir: meta.is_dir(),
            size: meta.len(),
        });
    }
    out.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });
    Ok(out)
}

pub fn browse_dir_unrestricted(path: &str) -> Result<Vec<DirEntry>> {
    let p = Path::new(path);
    if !p.is_absolute() {
        return Err(Error::Invalid(format!("path must be absolute: {}", path)));
    }
    reject_dotdot_components(p, path)?;
    let canon = p.canonicalize()?;
    if !canon.is_dir() {
        return Err(Error::Invalid(format!("not a directory: {}", path)));
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&canon)? {
        let e = entry?;
        let file_type = e.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let meta = e.metadata()?;
        if !meta.is_dir() {
            continue;
        }
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        out.push(DirEntry {
            name,
            path: e.path().to_string_lossy().to_string(),
            is_dir: true,
            size: 0,
        });
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(out)
}

pub fn read_file(path: &str, allowed_roots: &[PathBuf]) -> Result<String> {
    let p = validate_existing_workspace_path(path, allowed_roots)?;
    if !p.exists() {
        return Err(Error::NotFound(format!("file {:?}", p)));
    }
    let meta = p.metadata()?;
    if !meta.is_file() {
        return Err(Error::Invalid(format!(
            "path is not a file: {}",
            p.display()
        )));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(Error::Invalid(format!(
            "file exceeds {} byte limit: {}",
            MAX_FILE_BYTES,
            p.display()
        )));
    }
    let bytes = std::fs::read(&p)?;
    if let Ok(s) = std::str::from_utf8(&bytes) {
        return Ok(s.to_string());
    }
    Err(Error::Invalid("file is not valid UTF-8".into()))
}

pub fn write_file(path: &str, content: &str, allowed_roots: &[PathBuf]) -> Result<()> {
    let p = validate_workspace_write_path(path, allowed_roots)?;
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&p, content.as_bytes())?;
    Ok(())
}

pub fn validate_existing_workspace_path(
    requested: &str,
    allowed_roots: &[PathBuf],
) -> Result<PathBuf> {
    let p = Path::new(requested);
    if !p.is_absolute() {
        return Err(Error::Invalid(format!(
            "path must be absolute: {}",
            requested
        )));
    }
    reject_dotdot_components(p, requested)?;
    let roots = canonicalize_allowed_roots(allowed_roots)?;
    let canon = p.canonicalize()?;
    if !roots.iter().any(|r| canon.starts_with(r)) {
        return Err(Error::Invalid(format!(
            "path {} outside workspace",
            requested
        )));
    }
    Ok(canon)
}

pub fn validate_workspace_write_path(
    requested: &str,
    allowed_roots: &[PathBuf],
) -> Result<PathBuf> {
    let p = Path::new(requested);
    if !p.is_absolute() {
        return Err(Error::Invalid(format!(
            "path must be absolute: {}",
            requested
        )));
    }
    reject_dotdot_components(p, requested)?;
    let roots = canonicalize_allowed_roots(allowed_roots)?;
    let canon = canonicalize_existing_or_parent(p)?;
    if !roots.iter().any(|r| canon.starts_with(r)) {
        return Err(Error::Invalid(format!(
            "path {} outside workspace",
            requested
        )));
    }
    Ok(canon)
}

pub fn canonicalize_allowed_roots(allowed_roots: &[PathBuf]) -> Result<Vec<PathBuf>> {
    if allowed_roots.is_empty() {
        return Err(Error::Invalid("workspace roots required".into()));
    }
    let mut roots = Vec::with_capacity(allowed_roots.len());
    for root in allowed_roots {
        if root.as_os_str().is_empty() {
            continue;
        }
        let canon = root.canonicalize().map_err(|e| {
            Error::Invalid(format!(
                "workspace root {} unavailable: {}",
                root.display(),
                e
            ))
        })?;
        if !canon.is_dir() {
            return Err(Error::Invalid(format!(
                "workspace root is not a directory: {}",
                canon.display()
            )));
        }
        roots.push(canon);
    }
    if roots.is_empty() {
        return Err(Error::Invalid("workspace roots required".into()));
    }
    Ok(roots)
}

pub fn to_workspace_relative(canonical_path: &Path, allowed_roots: &[PathBuf]) -> Result<PathBuf> {
    let roots = canonicalize_allowed_roots(allowed_roots)?;
    let root = roots
        .iter()
        .find(|r| canonical_path.starts_with(r))
        .ok_or_else(|| {
            Error::Invalid(format!(
                "path {} outside workspace",
                canonical_path.display()
            ))
        })?;
    let rel = canonical_path.strip_prefix(root).map_err(|_| {
        Error::Invalid(format!(
            "path {} outside workspace",
            canonical_path.display()
        ))
    })?;
    if rel.as_os_str().is_empty() {
        return Err(Error::Invalid(
            "path must point inside workspace root".into(),
        ));
    }
    Ok(rel.to_path_buf())
}

fn reject_dotdot_components(path: &Path, requested: &str) -> Result<()> {
    if path
        .components()
        .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(Error::Invalid(format!(
            "path must not contain traversal components: {}",
            requested
        )));
    }
    Ok(())
}

fn canonicalize_existing_or_parent(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return Ok(path.canonicalize()?);
    }
    let mut ancestor = path
        .parent()
        .ok_or_else(|| Error::Invalid(format!("path has no parent: {}", path.display())))?;
    while !ancestor.exists() {
        ancestor = ancestor
            .parent()
            .ok_or_else(|| Error::Invalid(format!("no existing parent for {}", path.display())))?;
    }
    let suffix = path
        .strip_prefix(ancestor)
        .map_err(|_| Error::Invalid(format!("cannot validate path suffix: {}", path.display())))?;
    if suffix
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(Error::Invalid(format!(
            "path suffix contains invalid components: {}",
            path.display()
        )));
    }
    Ok(ancestor.canonicalize()?.join(suffix))
}

/// Walk a directory recursively up to `max_files` entries. Excludes
/// `.git`, `node_modules`, `target`, `dist`, `.pnpm-store`, `.pigmemory`.
pub fn walk_files(
    root: &str,
    max_files: usize,
    allowed_roots: &[PathBuf],
) -> Result<Vec<DirEntry>> {
    let root_path = validate_existing_workspace_path(root, allowed_roots)?;
    if !root_path.exists() {
        return Err(Error::NotFound(format!("path {}", root)));
    }
    let mut out = Vec::new();
    let mut stack = vec![root_path.clone()];
    while let Some(dir) = stack.pop() {
        if out.len() >= max_files {
            break;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(it) => it,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            if out.len() >= max_files {
                break;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if matches!(
                name.as_str(),
                ".git" | "node_modules" | "target" | "dist" | ".pnpm-store" | ".pigmemory"
            ) {
                continue;
            }
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            if file_type.is_symlink() {
                continue;
            }
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.is_dir() {
                stack.push(path);
            } else {
                out.push(DirEntry {
                    name,
                    path: path.to_string_lossy().to_string(),
                    is_dir: false,
                    size: meta.len(),
                });
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_rejects_empty_roots() {
        let root = tempdir_for_test("pigide-files-empty-roots");
        let file = root.join("note.txt");
        std::fs::write(&file, "secret").unwrap();

        let err = read_file(&file.to_string_lossy(), &[])
            .unwrap_err()
            .to_string();
        assert!(err.contains("workspace roots required"));

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn list_rejects_empty_roots() {
        let root = tempdir_for_test("pigide-files-list-empty-roots");

        let err = list_dir(&root.to_string_lossy(), &[])
            .unwrap_err()
            .to_string();
        assert!(err.contains("workspace roots required"));

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn read_rejects_parent_traversal() {
        let root = tempdir_for_test("pigide-files-read-root");
        let outside = root
            .parent()
            .unwrap()
            .join(format!("pigide-files-outside-{}", uuid::Uuid::new_v4()));
        std::fs::write(&outside, "outside").unwrap();
        let request = root.join("..").join(outside.file_name().unwrap());

        let err = read_file(&request.to_string_lossy(), std::slice::from_ref(&root))
            .unwrap_err()
            .to_string();
        assert!(err.contains("traversal"));

        std::fs::remove_dir_all(root).ok();
        std::fs::remove_file(outside).ok();
    }

    #[test]
    fn write_rejects_nonexistent_path_traversal() {
        let root = tempdir_for_test("pigide-files-write-root");
        let outside = root.parent().unwrap().join(format!(
            "pigide-files-write-outside-{}",
            uuid::Uuid::new_v4()
        ));
        let request = root.join("..").join(outside.file_name().unwrap());

        let err = write_file(
            &request.to_string_lossy(),
            "owned",
            std::slice::from_ref(&root),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("traversal"));
        assert!(!outside.exists());

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn write_allows_new_file_inside_workspace() {
        let root = tempdir_for_test("pigide-files-write-ok");
        let file = root.join("nested").join("note.txt");

        write_file(&file.to_string_lossy(), "ok", std::slice::from_ref(&root)).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "ok");

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn walk_rejects_paths_outside_workspace() {
        let root = tempdir_for_test("pigide-files-walk-root");
        let outside = root.parent().unwrap().join(format!(
            "pigide-files-walk-outside-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&outside).unwrap();

        let err = walk_files(&outside.to_string_lossy(), 10, std::slice::from_ref(&root))
            .unwrap_err()
            .to_string();
        assert!(err.contains("outside workspace"));

        std::fs::remove_dir_all(root).ok();
        std::fs::remove_dir_all(outside).ok();
    }

    fn tempdir_for_test(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("{}-{}", label, uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
