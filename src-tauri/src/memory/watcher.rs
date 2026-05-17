//! File-system watcher for `.pigmemory/` directories.
//!
//! Reads markdown files when they change on disk and re-indexes them through
//! `MemoryService`. Debounced (500 ms) to coalesce editor saves.
//!
//! Limitations: each watcher tracks one workspace. Switching workspaces in
//! the UI does not currently rebind — reattach on workspace change is a
//! follow-up.

use crate::error::{Error, Result};
use crate::memory::{note, storage, MemoryService};
use crate::workspace::WorkspaceManager;
use notify::{RecursiveMode, Watcher};
use notify_debouncer_full::{new_debouncer, DebouncedEvent};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Spawn a background watcher for the given workspace's `.pigmemory/` root.
/// Returns immediately; the watcher lives until the returned handle is
/// dropped (we leak it for the app lifetime via `Box::leak` in `setup`).
pub fn spawn(
    memory: Arc<MemoryService>,
    ws_mgr: Arc<WorkspaceManager>,
    workspace_id: String,
) -> Result<()> {
    let root = storage::resolve_root(&ws_mgr, &workspace_id)?;
    storage::ensure_root(&root)?;
    let root_clone = root.clone();
    let mem = memory.clone();
    let ws_id = workspace_id.clone();

    std::thread::Builder::new()
        .name("pigmemory-watcher".into())
        .spawn(move || {
            // Channel + debouncer.
            let (tx, rx) = std::sync::mpsc::channel();
            let mut debouncer = match new_debouncer(Duration::from_millis(500), None, tx) {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!("memory watcher: failed to init: {}", e);
                    return;
                }
            };
            if let Err(e) = debouncer.watcher().watch(&root_clone, RecursiveMode::Recursive) {
                tracing::warn!("memory watcher: watch failed for {:?}: {}", root_clone, e);
                return;
            }
            tracing::info!("memory watcher attached to {:?}", root_clone);

            for events in rx {
                let events = match events {
                    Ok(v) => v,
                    Err(errs) => {
                        for e in errs {
                            tracing::debug!("watch error: {:?}", e);
                        }
                        continue;
                    }
                };
                for ev in events {
                    handle_event(&mem, &ws_id, &root_clone, &ev);
                }
            }
        })
        .map_err(|e| Error::Other(format!("watcher thread: {}", e)))?;
    Ok(())
}

fn handle_event(
    memory: &Arc<MemoryService>,
    workspace_id: &str,
    root: &Path,
    ev: &DebouncedEvent,
) {
    use notify::EventKind;
    let paths: Vec<PathBuf> = ev.event.paths.clone();
    for path in paths {
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        match ev.event.kind {
            EventKind::Create(_) | EventKind::Modify(_) => {
                if let Err(e) = reindex_one(memory, workspace_id, root, &path) {
                    tracing::warn!("reindex {:?}: {}", path, e);
                }
            }
            EventKind::Remove(_) => {
                if let Err(e) = remove_by_path(memory, &path) {
                    tracing::warn!("remove {:?}: {}", path, e);
                }
            }
            _ => {}
        }
    }
}

fn reindex_one(
    memory: &Arc<MemoryService>,
    workspace_id: &str,
    root: &Path,
    path: &Path,
) -> Result<()> {
    let raw = match note::read(path) {
        Ok(s) => s,
        Err(_) => return Ok(()), // file gone between events
    };
    let slug = match storage::path_to_slug(root, path) {
        Some(s) => s,
        None => return Ok(()),
    };
    let parsed = note::parse(&slug, &raw)?;
    memory.reindex_from_disk(workspace_id, root, path, parsed)?;
    Ok(())
}

fn remove_by_path(memory: &Arc<MemoryService>, path: &Path) -> Result<()> {
    memory.delete_by_path(path)?;
    Ok(())
}
