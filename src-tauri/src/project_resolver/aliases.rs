//! Per-project alias store at `<project>/.pigmemory/aliases.json`.
//!
//! Format:
//! ```jsonc
//! { "aliases": ["widget", "widget plugin"] }
//! ```
//!
//! The file is per-project (not per-workspace) so aliases survive
//! workspace recreation and can be discovered by the indexer just by
//! walking the filesystem.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AliasFile {
    #[serde(default)]
    pub aliases: Vec<String>,
}

fn alias_path(project: &Path) -> PathBuf {
    project.join(".pigmemory").join("aliases.json")
}

pub fn load(project: &Path) -> Vec<String> {
    let path = alias_path(project);
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let af: AliasFile = match serde_json::from_str(&raw) {
        Ok(a) => a,
        Err(_) => return Vec::new(),
    };
    let mut seen = Vec::with_capacity(af.aliases.len());
    for a in af.aliases {
        let a = a.trim().to_string();
        if !a.is_empty() && !seen.iter().any(|x: &String| x.eq_ignore_ascii_case(&a)) {
            seen.push(a);
        }
    }
    seen
}

pub fn save(project: &Path, aliases: &[String]) -> Result<()> {
    let path = alias_path(project);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let af = AliasFile {
        aliases: aliases.to_vec(),
    };
    let raw = serde_json::to_string_pretty(&af)?;
    std::fs::write(&path, raw)?;
    Ok(())
}

pub fn add(project: &Path, alias: &str) -> Result<Vec<String>> {
    let alias = alias.trim();
    if alias.is_empty() {
        return Err(Error::Invalid("alias must be non-empty".into()));
    }
    if !project.is_dir() {
        return Err(Error::NotFound(format!(
            "project path not a directory: {}",
            project.display()
        )));
    }
    let mut current = load(project);
    if !current.iter().any(|a| a.eq_ignore_ascii_case(alias)) {
        current.push(alias.to_string());
    }
    save(project, &current)?;
    Ok(current)
}

pub fn remove(project: &Path, alias: &str) -> Result<Vec<String>> {
    let mut current = load(project);
    current.retain(|a| !a.eq_ignore_ascii_case(alias));
    save(project, &current)?;
    Ok(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let p =
            std::env::temp_dir().join(format!("pigide-resolver-aliases-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn empty_when_missing() {
        let d = tmp();
        assert!(load(&d).is_empty());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn add_then_load() {
        let d = tmp();
        let saved = add(&d, "Drugs Plugin").unwrap();
        assert_eq!(saved, vec!["Drugs Plugin".to_string()]);
        assert_eq!(load(&d), vec!["Drugs Plugin".to_string()]);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn add_dedupes_case_insensitive() {
        let d = tmp();
        add(&d, "widget").unwrap();
        let saved = add(&d, "WIDGET").unwrap();
        assert_eq!(saved.len(), 1);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn remove_works() {
        let d = tmp();
        add(&d, "widget").unwrap();
        add(&d, "плагин").unwrap();
        let saved = remove(&d, "WIDGET").unwrap();
        assert_eq!(saved, vec!["плагин".to_string()]);
        std::fs::remove_dir_all(&d).ok();
    }
}
