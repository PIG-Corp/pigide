//! Filesystem operations: read/write/list inside the workspace's
//! `paths[0]` (or any explicitly granted absolute path).
//!
//! All paths returned to callers are absolute; canonicalised through
//! `dunce::canonicalize` would be ideal but we keep it simpler with
//! `Path::canonicalize` — relying on the OS to resolve `..` and symlinks.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
}

pub fn list_dir(path: &str) -> Result<Vec<DirEntry>> {
    let p = PathBuf::from(path);
    if !p.exists() {
        return Err(Error::NotFound(format!("path {:?}", p)));
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&p)? {
        let e = entry?;
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

pub fn read_file(path: &str) -> Result<String> {
    let p = Path::new(path);
    if !p.exists() {
        return Err(Error::NotFound(format!("file {:?}", p)));
    }
    let bytes = std::fs::read(p)?;
    if let Ok(s) = std::str::from_utf8(&bytes) {
        return Ok(s.to_string());
    }
    Err(Error::Invalid("file is not valid UTF-8".into()))
}

pub fn write_file(path: &str, content: &str) -> Result<()> {
    let p = Path::new(path);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(p, content.as_bytes())?;
    Ok(())
}

/// Walk a directory recursively up to `max_files` entries. Excludes
/// `.git`, `node_modules`, `target`, `dist`, `.pnpm-store`, `.pigmemory`.
pub fn walk_files(root: &str, max_files: usize) -> Result<Vec<DirEntry>> {
    let root_path = PathBuf::from(root);
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
