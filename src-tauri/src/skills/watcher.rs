//! File-system watcher for skills roots.
//!
//! Each [`SkillSource`] gets one watcher (debounced 250 ms). Events trigger
//! a per-path reload through [`SkillRegistry::reload_path`]. Failures are
//! logged and surfaced via `skills_registry.last_errors()` + the
//! `skills://reloaded` Tauri event.

use crate::error::Result;
use crate::skills::registry::{SkillRegistry, SkillSource};
use notify::{RecursiveMode, Watcher};
use notify_debouncer_full::new_debouncer;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter};

pub const EV_SKILLS_RELOADED: &str = "skills://reloaded";
pub const EV_SKILLS_ERROR: &str = "skills://error";

/// Spawn a background watcher for every existing source root. The watchers
/// run for the lifetime of the process — there is one global registry per
/// app, so this is fine.
pub fn spawn_all(
    app: AppHandle,
    registry: Arc<SkillRegistry>,
    sources: Vec<SkillSource>,
) -> Result<()> {
    for src in sources {
        if !src.root.exists() {
            // Create user dir on demand so the user can drop files in.
            if matches!(src.tag, crate::skills::skill::SkillSourceTag::User) {
                let _ = std::fs::create_dir_all(&src.root);
            } else {
                continue;
            }
        }
        let app_for_thread = app.clone();
        let reg = registry.clone();
        let root = src.root.clone();
        std::thread::Builder::new()
            .name(format!("pigide-skills-watch-{}", src.tag.as_str()))
            .spawn(move || {
                let (tx, rx) = std::sync::mpsc::channel();
                let mut deb = match new_debouncer(Duration::from_millis(250), None, tx) {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::warn!("skills watcher init: {}", e);
                        return;
                    }
                };
                if let Err(e) =
                    deb.watcher().watch(&root, RecursiveMode::Recursive)
                {
                    tracing::warn!(
                        "skills watcher: watch {} failed: {}",
                        root.display(),
                        e
                    );
                    return;
                }
                tracing::info!("skills watcher attached to {}", root.display());
                for batch in rx {
                    let events = match batch {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    for ev in events {
                        for path in ev.event.paths.iter() {
                            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                                continue;
                            }
                            match reg.reload_path(path) {
                                Ok(()) => {
                                    let _ = app_for_thread.emit(
                                        EV_SKILLS_RELOADED,
                                        json!({"path": path.display().to_string()}),
                                    );
                                }
                                Err(e) => {
                                    let _ = app_for_thread.emit(
                                        EV_SKILLS_ERROR,
                                        json!({
                                            "path": path.display().to_string(),
                                            "error": e.to_string()
                                        }),
                                    );
                                }
                            }
                        }
                    }
                }
            })?;
    }
    Ok(())
}
