//! Per-project metadata extraction from on-disk signals.
//!
//! Each parser is best-effort and infallible from the caller's POV: a
//! malformed `package.json` should never abort indexing — it just yields
//! no signals.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Aggregated metadata extracted from a single project root.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectSignals {
    pub names: Vec<String>,
    pub descriptions: Vec<String>,
    pub headings: Vec<String>,
    pub remote: Option<String>,
    pub languages: Vec<String>,
    pub kinds: Vec<String>,
}

impl ProjectSignals {
    pub fn merge(&mut self, other: ProjectSignals) {
        for n in other.names {
            if !self.names.contains(&n) {
                self.names.push(n);
            }
        }
        for d in other.descriptions {
            if !self.descriptions.contains(&d) {
                self.descriptions.push(d);
            }
        }
        for h in other.headings {
            if !self.headings.contains(&h) {
                self.headings.push(h);
            }
        }
        if self.remote.is_none() {
            self.remote = other.remote;
        }
        for l in other.languages {
            if !self.languages.contains(&l) {
                self.languages.push(l);
            }
        }
        for k in other.kinds {
            if !self.kinds.contains(&k) {
                self.kinds.push(k);
            }
        }
    }
}

const PROJECT_MARKERS: &[&str] = &[
    ".git",
    "package.json",
    "Cargo.toml",
    "pyproject.toml",
    "go.mod",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    ".pigide",
    "paper-plugin.yml",
    "plugin.yml",
];

/// True if `dir` looks like a project root.
pub fn is_project_root(dir: &Path) -> bool {
    PROJECT_MARKERS.iter().any(|m| dir.join(m).exists())
}

/// Run every applicable parser against `dir` and merge their output.
pub fn parse_all(dir: &Path) -> ProjectSignals {
    let mut out = ProjectSignals::default();
    if dir.join(".git").exists() {
        out.kinds.push("git".into());
        if let Some(remote) = parse_git_remote(dir) {
            out.remote = Some(remote);
        }
    }
    if let Some(s) = parse_package_json(dir) {
        out.kinds.push("node".into());
        out.languages.push("javascript".into());
        out.merge(s);
    }
    if let Some(s) = parse_cargo_toml(dir) {
        out.kinds.push("rust".into());
        out.languages.push("rust".into());
        out.merge(s);
    }
    if let Some(s) = parse_pyproject(dir) {
        out.kinds.push("python".into());
        out.languages.push("python".into());
        out.merge(s);
    }
    if let Some(s) = parse_go_mod(dir) {
        out.kinds.push("go".into());
        out.languages.push("go".into());
        out.merge(s);
    }
    if let Some(s) = parse_paper_plugin_yml(dir) {
        out.kinds.push("paper-plugin".into());
        out.languages.push("java".into());
        out.merge(s);
    }
    if let Some(headings) = parse_readme_headings(dir) {
        out.headings.extend(headings);
    }
    out
}

// ----- individual parsers --------------------------------------------------

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn parse_package_json(dir: &Path) -> Option<ProjectSignals> {
    let raw = read(&dir.join("package.json"))?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let mut out = ProjectSignals::default();
    if let Some(name) = v.get("name").and_then(|n| n.as_str()) {
        out.names.push(strip_npm_scope(name));
    }
    if let Some(desc) = v.get("description").and_then(|n| n.as_str()) {
        out.descriptions.push(desc.to_string());
    }
    Some(out)
}

fn strip_npm_scope(name: &str) -> String {
    if let Some(rest) = name.strip_prefix('@') {
        if let Some((_, n)) = rest.split_once('/') {
            return n.to_string();
        }
    }
    name.to_string()
}

fn parse_cargo_toml(dir: &Path) -> Option<ProjectSignals> {
    let raw = read(&dir.join("Cargo.toml"))?;
    let mut out = ProjectSignals::default();
    let mut in_pkg = false;
    for line in raw.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            in_pkg = l == "[package]";
            continue;
        }
        if !in_pkg {
            continue;
        }
        if let Some(v) = trim_assign(l, "name") {
            out.names.push(v);
        } else if let Some(v) = trim_assign(l, "description") {
            out.descriptions.push(v);
        }
    }
    if out.names.is_empty() && out.descriptions.is_empty() {
        return None;
    }
    Some(out)
}

fn parse_pyproject(dir: &Path) -> Option<ProjectSignals> {
    let raw = read(&dir.join("pyproject.toml"))?;
    let mut out = ProjectSignals::default();
    let mut section: Option<String> = None;
    for line in raw.lines() {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            section = Some(rest.to_string());
            continue;
        }
        let in_meta = matches!(section.as_deref(), Some("project") | Some("tool.poetry"));
        if !in_meta {
            continue;
        }
        if let Some(v) = trim_assign(l, "name") {
            out.names.push(v);
        } else if let Some(v) = trim_assign(l, "description") {
            out.descriptions.push(v);
        }
    }
    if out.names.is_empty() && out.descriptions.is_empty() {
        return None;
    }
    Some(out)
}

fn parse_go_mod(dir: &Path) -> Option<ProjectSignals> {
    let raw = read(&dir.join("go.mod"))?;
    for line in raw.lines() {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix("module") {
            let module = rest.trim().trim_matches('"').to_string();
            let leaf = module.rsplit('/').next().unwrap_or(&module).to_string();
            let mut out = ProjectSignals::default();
            out.names.push(leaf);
            out.names.push(module);
            return Some(out);
        }
    }
    None
}

fn parse_paper_plugin_yml(dir: &Path) -> Option<ProjectSignals> {
    for fname in &["paper-plugin.yml", "plugin.yml"] {
        let path = dir.join(fname);
        if !path.exists() {
            continue;
        }
        let raw = read(&path)?;
        let mut out = ProjectSignals::default();
        for line in raw.lines() {
            let l = line.trim();
            if let Some(v) = trim_yaml(l, "name") {
                out.names.push(v);
            } else if let Some(v) = trim_yaml(l, "description") {
                out.descriptions.push(v);
            }
        }
        if !out.names.is_empty() || !out.descriptions.is_empty() {
            return Some(out);
        }
    }
    None
}

fn parse_readme_headings(dir: &Path) -> Option<Vec<String>> {
    for name in &["README.md", "README.MD", "Readme.md", "readme.md", "README"] {
        let path = dir.join(name);
        if !path.exists() {
            continue;
        }
        let raw = read(&path)?;
        let mut out = Vec::new();
        for line in raw.lines().take(80) {
            let l = line.trim_start();
            if let Some(h) = l.strip_prefix("# ") {
                out.push(h.trim().to_string());
            } else if let Some(h) = l.strip_prefix("## ") {
                out.push(h.trim().to_string());
            }
            if out.len() >= 3 {
                break;
            }
        }
        if !out.is_empty() {
            return Some(out);
        }
    }
    None
}

fn parse_git_remote(dir: &Path) -> Option<String> {
    let cfg = read(&dir.join(".git").join("config"))?;
    let mut in_origin = false;
    for line in cfg.lines() {
        let l = line.trim();
        if l.starts_with("[remote") {
            in_origin = l.contains("\"origin\"");
            continue;
        }
        if !in_origin {
            continue;
        }
        if let Some(rest) = l.strip_prefix("url") {
            let url = rest.trim_start_matches([' ', '=']).trim().to_string();
            return Some(normalize_remote(&url));
        }
    }
    None
}

fn normalize_remote(url: &str) -> String {
    // git@github.com:owner/repo.git -> github.com/owner/repo
    let url = url.trim_end_matches(".git");
    if let Some(rest) = url.strip_prefix("git@") {
        if let Some((host, path)) = rest.split_once(':') {
            return format!("{}/{}", host, path);
        }
    }
    if let Some(rest) = url.strip_prefix("https://") {
        return rest.to_string();
    }
    if let Some(rest) = url.strip_prefix("http://") {
        return rest.to_string();
    }
    url.to_string()
}

/// Extract `key = "value"` from a TOML-ish line. Lossy on purpose.
fn trim_assign(line: &str, key: &str) -> Option<String> {
    let line = line.split('#').next().unwrap_or(line).trim();
    let rest = line.strip_prefix(key)?.trim_start();
    let rest = rest.strip_prefix('=')?.trim();
    let v = rest
        .trim_matches(|c| c == '"' || c == '\'')
        .trim()
        .to_string();
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

/// Extract `key: value` from a YAML-ish line. Lossy.
fn trim_yaml(line: &str, key: &str) -> Option<String> {
    let line = line.split('#').next().unwrap_or(line).trim();
    let rest = line.strip_prefix(key)?.trim_start();
    let rest = rest.strip_prefix(':')?.trim();
    let v = rest
        .trim_matches(|c| c == '"' || c == '\'')
        .trim()
        .to_string();
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

/// Helper: dirname.
pub fn dirname(p: &Path) -> String {
    p.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string()
}

/// Helper: extract owner/repo leaf from a normalized remote like
/// `github.com/owner/repo`. Returns None if it doesn't parse.
pub fn remote_repo_name(remote: &str) -> Option<String> {
    let parts: Vec<&str> = remote.split('/').collect();
    parts.last().map(|s| s.to_string())
}

/// Convenience constructor used by tests in higher modules.
pub fn parse_dir(dir: &Path) -> ProjectSignals {
    parse_all(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;

    fn tmp() -> PathBuf {
        let p =
            std::env::temp_dir().join(format!("pigide-resolver-parsers-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn package_json_name_and_desc() {
        let d = tmp();
        let mut f = fs::File::create(d.join("package.json")).unwrap();
        write!(
            f,
            r#"{{ "name": "@scope/cool-tool", "description": "does cool things" }}"#
        )
        .unwrap();
        let s = parse_all(&d);
        assert!(s.names.contains(&"cool-tool".to_string()));
        assert!(s.descriptions.contains(&"does cool things".to_string()));
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn cargo_toml_pkg() {
        let d = tmp();
        let mut f = fs::File::create(d.join("Cargo.toml")).unwrap();
        writeln!(f, "[package]\nname = \"my_crate\"\ndescription = \"crate\"").unwrap();
        let s = parse_all(&d);
        assert!(s.names.contains(&"my_crate".to_string()));
        assert!(s.descriptions.contains(&"crate".to_string()));
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn pyproject_project_section() {
        let d = tmp();
        let mut f = fs::File::create(d.join("pyproject.toml")).unwrap();
        writeln!(
            f,
            "[project]\nname = \"datapipe\"\ndescription = \"etl tool\""
        )
        .unwrap();
        let s = parse_all(&d);
        assert!(s.names.contains(&"datapipe".to_string()));
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn go_mod_module() {
        let d = tmp();
        let mut f = fs::File::create(d.join("go.mod")).unwrap();
        writeln!(f, "module github.com/acme/widget\ngo 1.21").unwrap();
        let s = parse_all(&d);
        assert!(s.names.iter().any(|n| n == "widget"));
        assert!(s.names.iter().any(|n| n == "github.com/acme/widget"));
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn readme_h1_picked() {
        let d = tmp();
        let mut f = fs::File::create(d.join("README.md")).unwrap();
        writeln!(f, "# Drugs Plugin\n\nlong text here").unwrap();
        let s = parse_all(&d);
        assert!(s.headings.iter().any(|h| h == "Drugs Plugin"));
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn project_root_detection() {
        let d = tmp();
        assert!(!is_project_root(&d));
        fs::write(d.join("Cargo.toml"), "[package]\nname=\"x\"").unwrap();
        assert!(is_project_root(&d));
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn paper_plugin_yml_picked() {
        let d = tmp();
        fs::write(
            d.join("paper-plugin.yml"),
            "name: DrugsPlugin\nversion: 1.0\ndescription: tracks drugs\n",
        )
        .unwrap();
        let s = parse_all(&d);
        assert!(s.names.iter().any(|n| n == "DrugsPlugin"));
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn malformed_package_json_no_panic() {
        let d = tmp();
        fs::write(d.join("package.json"), "{ this is not json").unwrap();
        let _ = parse_all(&d);
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn normalize_remote_ssh() {
        assert_eq!(
            normalize_remote("git@github.com:foo/bar.git"),
            "github.com/foo/bar"
        );
        assert_eq!(
            normalize_remote("https://gitlab.example/foo/bar"),
            "gitlab.example/foo/bar"
        );
    }
}
