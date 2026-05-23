//! In-memory skill registry: discovery, source precedence, hot-reload.
//!
//! Three sources are checked in increasing precedence:
//!   1. Built-in   — shipped with the binary at `<exe>/../resources/skills`
//!      (or `CARGO_MANIFEST_DIR/resources/skills` at dev time).
//!   2. User       — `~/.pigide/skills/`.
//!   3. Workspace  — `<workspace.path[0]>/.pigide/skills/`.
//!
//! When the same `id` appears in more than one source, the higher-precedence
//! one wins. The shadowed copies are kept and exposed via [`SkillEntry::shadowed_by`].

use crate::error::{Error, Result};
use crate::skills::skill::{parse, Skill, SkillSourceTag};
use parking_lot::RwLock;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct SkillSource {
    pub tag: SkillSourceTag,
    pub root: PathBuf,
}

/// What we expose to the UI: a public-facing entry that may be shadowed.
#[derive(Debug, Clone)]
pub struct SkillEntry {
    pub skill: Skill,
    pub shadowed_by: Option<SkillSourceTag>,
    /// True when an explicit override in `settings`
    /// (key `skills.disabled.<id>` = "true") disables this skill regardless
    /// of `frontmatter.enabled`.
    pub override_disabled: bool,
}

impl SkillEntry {
    pub fn effective_enabled(&self) -> bool {
        self.skill.frontmatter.enabled && !self.override_disabled && self.shadowed_by.is_none()
    }
}

#[derive(Default, Debug)]
struct Inner {
    /// All loaded skills, keyed by `(source, id)`. We keep the shadowed
    /// copies so the UI can show which file is winning.
    by_key: HashMap<(SkillSourceTag, String), Skill>,
    /// Per-id list of sources sorted by precedence (highest first).
    by_id: HashMap<String, Vec<SkillSourceTag>>,
    /// Manual override map: id -> disabled?
    overrides: HashMap<String, bool>,
    /// Errors observed during the last full scan, surfaced in the UI.
    last_errors: Vec<LoadError>,
}

#[derive(Debug, Clone)]
pub struct LoadError {
    pub path: String,
    pub source: SkillSourceTag,
    pub error: String,
}

#[derive(Default)]
pub struct SkillRegistry {
    inner: RwLock<Inner>,
    sources: RwLock<Vec<SkillSource>>,
}

impl SkillRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(SkillRegistry::default())
    }

    /// Replace the configured sources. Call after computing roots from
    /// settings + workspace.
    pub fn set_sources(&self, sources: Vec<SkillSource>) {
        *self.sources.write() = sources;
    }

    pub fn sources(&self) -> Vec<SkillSource> {
        self.sources.read().clone()
    }

    /// Replace manual disable overrides (id -> disabled).
    pub fn set_overrides(&self, overrides: HashMap<String, bool>) {
        self.inner.write().overrides = overrides;
    }

    pub fn override_for(&self, id: &str) -> Option<bool> {
        self.inner.read().overrides.get(id).copied()
    }

    /// Force a full rescan from disk.
    pub fn reload_all(&self) -> Result<()> {
        let sources = self.sources.read().clone();
        let mut by_key: HashMap<(SkillSourceTag, String), Skill> = HashMap::new();
        let mut errors: Vec<LoadError> = Vec::new();
        for src in &sources {
            if !src.root.exists() {
                continue;
            }
            walk(&src.root, &mut |path| {
                if path.extension().and_then(|s| s.to_str()) != Some("md") {
                    return;
                }
                let raw = match std::fs::read_to_string(path) {
                    Ok(s) => s,
                    Err(e) => {
                        errors.push(LoadError {
                            path: path.display().to_string(),
                            source: src.tag,
                            error: format!("read: {}", e),
                        });
                        return;
                    }
                };
                let path_str = path.display().to_string();
                match parse(&path_str, src.tag, &raw) {
                    Ok(Some(sk)) => {
                        by_key.insert((src.tag, sk.id.clone()), sk);
                    }
                    Ok(None) => {} // not a skill (no frontmatter)
                    Err(e) => errors.push(LoadError {
                        path: path_str,
                        source: src.tag,
                        error: e.to_string(),
                    }),
                }
            })?;
        }

        // Build by_id index sorted by precedence (highest first).
        let mut by_id: HashMap<String, Vec<SkillSourceTag>> = HashMap::new();
        for (src, id) in by_key.keys() {
            by_id.entry(id.clone()).or_default().push(*src);
        }
        for v in by_id.values_mut() {
            v.sort_by_key(|b| std::cmp::Reverse(b.precedence()));
        }

        let mut inner = self.inner.write();
        inner.by_key = by_key;
        inner.by_id = by_id;
        inner.last_errors = errors;
        Ok(())
    }

    /// Re-load a single file (used by the hot-reload watcher).
    pub fn reload_path(&self, path: &Path) -> Result<()> {
        let source = match self.classify_source(path) {
            Some(s) => s,
            None => return Ok(()),
        };
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            return Ok(());
        }
        if !path.exists() {
            // Treat as removal: drop matching entry.
            let mut inner = self.inner.write();
            // We need to find which (source, id) had this path.
            let key_to_drop: Option<(SkillSourceTag, String)> =
                inner.by_key.iter().find_map(|((s, id), sk)| {
                    if sk.path == path.display().to_string() {
                        Some((*s, id.clone()))
                    } else {
                        None
                    }
                });
            if let Some(k) = key_to_drop {
                inner.by_key.remove(&k);
                if let Some(v) = inner.by_id.get_mut(&k.1) {
                    v.retain(|s| *s != k.0);
                    if v.is_empty() {
                        inner.by_id.remove(&k.1);
                    }
                }
            }
            return Ok(());
        }
        let raw = std::fs::read_to_string(path)
            .map_err(|e| Error::Other(format!("read {}: {}", path.display(), e)))?;
        let path_str = path.display().to_string();
        let parsed = parse(&path_str, source, &raw);
        let mut inner = self.inner.write();
        match parsed {
            Ok(Some(sk)) => {
                let id = sk.id.clone();
                inner.by_key.insert((source, id.clone()), sk);
                inner
                    .by_id
                    .entry(id)
                    .or_default()
                    .iter()
                    .find(|s| **s == source)
                    .cloned()
                    .unwrap_or_else(|| {
                        let v = inner.by_id.entry(String::new()).or_default();
                        let _ = v;
                        source
                    });
                // Rebuild by_id list for that id to keep sort invariant.
                let id_key = inner
                    .by_key
                    .keys()
                    .find(|(s, _)| *s == source)
                    .map(|(_, id)| id.clone());
                if let Some(id_str) = id_key {
                    let mut sources_for_id: Vec<SkillSourceTag> = inner
                        .by_key
                        .keys()
                        .filter(|(_, i)| *i == id_str)
                        .map(|(s, _)| *s)
                        .collect();
                    sources_for_id.sort_by_key(|b| std::cmp::Reverse(b.precedence()));
                    inner.by_id.insert(id_str, sources_for_id);
                }
                Ok(())
            }
            Ok(None) => Ok(()),
            Err(e) => {
                inner.last_errors.push(LoadError {
                    path: path_str,
                    source,
                    error: e.to_string(),
                });
                Ok(())
            }
        }
    }

    fn classify_source(&self, path: &Path) -> Option<SkillSourceTag> {
        let sources = self.sources.read();
        for s in sources.iter() {
            if path.starts_with(&s.root) {
                return Some(s.tag);
            }
        }
        None
    }

    /// Effective view: for each id, the winning skill plus shadowing info.
    pub fn entries(&self) -> Vec<SkillEntry> {
        let inner = self.inner.read();
        let mut out: Vec<SkillEntry> = Vec::new();
        let mut seen_ids: BTreeMap<String, &Skill> = BTreeMap::new();

        // Active winners.
        for (id, sources) in &inner.by_id {
            if let Some(top) = sources.first() {
                if let Some(sk) = inner.by_key.get(&(*top, id.clone())) {
                    seen_ids.insert(id.clone(), sk);
                    out.push(SkillEntry {
                        skill: sk.clone(),
                        shadowed_by: None,
                        override_disabled: inner.overrides.get(id).copied().unwrap_or(false),
                    });
                }
            }
            // Shadowed copies (everything but the winner).
            for shadow_src in sources.iter().skip(1) {
                if let Some(sk) = inner.by_key.get(&(*shadow_src, id.clone())) {
                    let winner_src = sources[0];
                    out.push(SkillEntry {
                        skill: sk.clone(),
                        shadowed_by: Some(winner_src),
                        override_disabled: inner.overrides.get(id).copied().unwrap_or(false),
                    });
                }
            }
        }
        out.sort_by(|a, b| {
            // Stable: id, then source precedence desc.
            a.skill.id.cmp(&b.skill.id).then(
                b.skill
                    .source
                    .precedence()
                    .cmp(&a.skill.source.precedence()),
            )
        });
        out
    }

    /// Active skills only (winners + enabled + not overridden).
    pub fn active(&self) -> Vec<Skill> {
        self.entries()
            .into_iter()
            .filter(|e| e.shadowed_by.is_none() && e.effective_enabled())
            .map(|e| e.skill)
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<Skill> {
        let inner = self.inner.read();
        let sources = inner.by_id.get(id)?;
        let top = sources.first()?;
        inner.by_key.get(&(*top, id.to_string())).cloned()
    }

    pub fn last_errors(&self) -> Vec<LoadError> {
        self.inner.read().last_errors.clone()
    }
}

/// Recursively walk `root`, calling `f` on every regular file. Symlinks
/// pointing outside `root` are ignored as a defence-in-depth measure.
fn walk(root: &Path, f: &mut dyn FnMut(&Path)) -> Result<()> {
    let canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    walk_inner(&canon, &canon, f);
    Ok(())
}

fn walk_inner(root: &Path, dir: &Path, f: &mut dyn FnMut(&Path)) {
    let rd = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };
    for entry in rd.flatten() {
        let path = entry.path();
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.file_type().is_symlink() {
            // Resolve and refuse if it escapes the root.
            let target = match std::fs::canonicalize(&path) {
                Ok(t) => t,
                Err(_) => continue,
            };
            if !target.starts_with(root) {
                continue;
            }
        }
        if meta.is_dir() {
            walk_inner(root, &path, f);
        } else if meta.is_file() {
            f(&path);
        }
    }
}

/// Resolve the three default roots given the user-configured user dir,
/// optional workspace path, and the dev/release split for built-ins.
pub fn default_sources(
    user_dir: Option<PathBuf>,
    workspace_dir: Option<PathBuf>,
) -> Vec<SkillSource> {
    let mut out = Vec::new();
    if let Some(p) = builtin_root() {
        out.push(SkillSource {
            tag: SkillSourceTag::Builtin,
            root: p,
        });
    }
    let user = user_dir.unwrap_or_else(default_user_dir);
    out.push(SkillSource {
        tag: SkillSourceTag::User,
        root: user,
    });
    if let Some(ws) = workspace_dir {
        out.push(SkillSource {
            tag: SkillSourceTag::Workspace,
            root: ws.join(".pigide").join("skills"),
        });
    }
    out
}

pub fn default_user_dir() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        home.join(".pigide").join("skills")
    } else {
        PathBuf::from(".pigide/skills")
    }
}

/// Best-effort discovery of the bundled `resources/skills` directory.
fn builtin_root() -> Option<PathBuf> {
    // Dev time: relative to the crate manifest.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("resources")
        .join("skills");
    if manifest.exists() {
        return Some(manifest);
    }
    // Release: next to the executable.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join("resources").join("skills");
            if p.exists() {
                return Some(p);
            }
            let p = dir.join("../resources/skills");
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    /// Bare-bones scoped temp dir — avoids pulling in `tempfile` for tests.
    struct Tmp {
        path: PathBuf,
    }
    impl Tmp {
        fn new(tag: &str) -> Self {
            let mut path = std::env::temp_dir();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            path.push(format!("pigide-skills-{}-{}", tag, nanos));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }
        fn path(&self) -> &Path {
            &self.path
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
    fn tmpdir() -> Tmp {
        Tmp::new("registry")
    }

    fn write_file(path: &Path, body: &str) {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).unwrap();
        }
        let mut f = fs::File::create(path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    const SAMPLE: &str = r#"---
id: hello
name: Hello
description: greets
---
Hi {{name}}!
"#;

    #[test]
    fn discovers_builtin() {
        let td = tmpdir();
        let root = td.path().to_path_buf();
        write_file(&root.join("hello.md"), SAMPLE);
        let reg = SkillRegistry::new();
        reg.set_sources(vec![SkillSource {
            tag: SkillSourceTag::Builtin,
            root,
        }]);
        reg.reload_all().unwrap();
        let active = reg.active();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, "hello");
    }

    #[test]
    fn workspace_shadows_builtin() {
        let bi = tmpdir();
        let ws = tmpdir();
        write_file(&bi.path().join("hello.md"), SAMPLE);
        let ws_body = SAMPLE.replace("name: Hello", "name: Hello (workspace)");
        write_file(&ws.path().join("hello.md"), &ws_body);

        let reg = SkillRegistry::new();
        reg.set_sources(vec![
            SkillSource {
                tag: SkillSourceTag::Builtin,
                root: bi.path().to_path_buf(),
            },
            SkillSource {
                tag: SkillSourceTag::Workspace,
                root: ws.path().to_path_buf(),
            },
        ]);
        reg.reload_all().unwrap();

        let entries = reg.entries();
        let winner = entries.iter().find(|e| e.shadowed_by.is_none()).unwrap();
        assert_eq!(winner.skill.source, SkillSourceTag::Workspace);
        assert!(winner.skill.frontmatter.name.contains("workspace"));

        let shadowed = entries
            .iter()
            .find(|e| e.shadowed_by == Some(SkillSourceTag::Workspace))
            .unwrap();
        assert_eq!(shadowed.skill.source, SkillSourceTag::Builtin);
    }

    #[test]
    fn override_disables_a_skill() {
        let td = tmpdir();
        write_file(&td.path().join("hello.md"), SAMPLE);
        let reg = SkillRegistry::new();
        reg.set_sources(vec![SkillSource {
            tag: SkillSourceTag::Builtin,
            root: td.path().to_path_buf(),
        }]);
        reg.reload_all().unwrap();
        let mut overrides = HashMap::new();
        overrides.insert("hello".into(), true);
        reg.set_overrides(overrides);
        let entries = reg.entries();
        let e = entries.iter().find(|e| e.skill.id == "hello").unwrap();
        assert!(!e.effective_enabled());
        assert!(reg.active().is_empty());
    }
}
