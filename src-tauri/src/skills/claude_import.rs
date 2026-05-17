//! Claude Code → PigIDE skills importer.
//!
//! Scans known Claude skill source roots (user-level `~/.claude/skills/`,
//! plugin caches under `~/.claude/plugins/`, and an in-repo `claude-skills/`
//! sibling, plus any explicit roots passed in by the caller), parses the
//! `SKILL.md` frontmatter, and writes a PigIDE-shaped `.md` skill into
//! `~/.pigide/skills/imported/<id>.md`.
//!
//! ### Field mapping (Claude → PigIDE)
//!
//! | Claude (frontmatter)   | PigIDE (frontmatter)                           |
//! |------------------------|------------------------------------------------|
//! | `name`                 | `id` (slugified, prefixed `claude-`) + `name`  |
//! | `description`          | `description`                                  |
//! | `allowed-tools` (list) | `tags: ["claude-tools:<tool>", ...]` (info)    |
//! | `model` / `model_hint` | `model_hint`                                   |
//! | (file body w/o FM)     | body                                           |
//! | (constant)             | `priority: 50`, `triggers: []`, `enabled: true`|
//!
//! Idempotency: re-running re-scans sources, recomputes id from `name`, and
//! overwrites the imported file. The original Claude path is stored in an
//! HTML-comment line at the top of the body so a re-sync can still find the
//! source. We never touch files outside the imported directory.

use crate::error::{Error, Result};
use crate::skills::skill::{split_frontmatter, SkillFrontmatter};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// One Claude skill source root, with a human-readable label.
#[derive(Debug, Clone, Serialize)]
pub struct ClaudeSourceRoot {
    pub label: String,
    pub path: String,
    pub exists: bool,
    pub skill_count: usize,
}

/// Result row for a single imported / skipped skill.
#[derive(Debug, Clone, Serialize)]
pub struct ImportedSkill {
    pub id: String,
    pub name: String,
    pub source_path: String,
    pub written_to: String,
    pub status: ImportStatus,
    /// Warnings (e.g., unmapped tool names) — does not fail the import.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ImportStatus {
    Created,
    Updated,
    Unchanged,
    Skipped,
    Failed,
}

/// Summary returned to the UI.
#[derive(Debug, Clone, Serialize)]
pub struct ImportReport {
    pub roots: Vec<ClaudeSourceRoot>,
    pub imported: Vec<ImportedSkill>,
    pub destination: String,
    pub created: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub skipped: usize,
    pub failed: usize,
}

/// Public entry point: discover sources (plus any extras the caller adds),
/// parse, map, write to `~/.pigide/skills/imported/`, return a report.
///
/// Pure file-system work — no tauri, no DB, no network. The caller is
/// expected to call `SkillRegistry::reload_all()` after this returns to
/// surface the new files in the registry.
pub fn import(extra_roots: &[PathBuf]) -> Result<ImportReport> {
    let dest_dir = imported_dir();
    std::fs::create_dir_all(&dest_dir)?;

    let mut roots = default_roots();
    for p in extra_roots {
        if !roots.iter().any(|r| Path::new(&r.path) == p.as_path()) {
            roots.push(ClaudeSourceRoot {
                label: format!("custom:{}", p.display()),
                path: p.display().to_string(),
                exists: p.exists(),
                skill_count: 0,
            });
        }
    }

    // Collect candidates first so we can dedupe by source path.
    let mut candidates: Vec<(PathBuf, String)> = Vec::new(); // (file, root_label)
    for root in roots.iter_mut() {
        if !root.exists {
            continue;
        }
        let mut found: Vec<PathBuf> = Vec::new();
        scan_root(Path::new(&root.path), &mut found);
        root.skill_count = found.len();
        for f in found {
            candidates.push((f, root.label.clone()));
        }
    }

    // Dedupe: when the same SKILL.md appears under multiple roots (e.g.
    // symlinks), keep the first.
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    let mut imported: Vec<ImportedSkill> = Vec::new();
    let (mut created, mut updated, mut unchanged, mut skipped, mut failed) =
        (0usize, 0usize, 0usize, 0usize, 0usize);

    for (path, _root_label) in candidates {
        let canon = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if !seen.insert(canon.clone()) {
            continue;
        }
        let row = match import_one(&path, &dest_dir) {
            Ok(r) => r,
            Err(e) => ImportedSkill {
                id: String::new(),
                name: path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("?")
                    .to_string(),
                source_path: path.display().to_string(),
                written_to: String::new(),
                status: ImportStatus::Failed,
                warnings: vec![format!("error: {}", e)],
            },
        };
        match row.status {
            ImportStatus::Created => created += 1,
            ImportStatus::Updated => updated += 1,
            ImportStatus::Unchanged => unchanged += 1,
            ImportStatus::Skipped => skipped += 1,
            ImportStatus::Failed => failed += 1,
        }
        imported.push(row);
    }

    imported.sort_by(|a, b| a.id.cmp(&b.id).then_with(|| a.name.cmp(&b.name)));
    Ok(ImportReport {
        roots,
        imported,
        destination: dest_dir.display().to_string(),
        created,
        updated,
        unchanged,
        skipped,
        failed,
    })
}

/// Where imported files live. Lives under the user source root so the
/// existing registry/watcher pick them up automatically.
pub fn imported_dir() -> PathBuf {
    crate::skills::registry::default_user_dir().join("imported")
}

/// Default search roots, in priority order. Non-existent paths are still
/// reported (with `exists: false`) so the UI can surface what was looked at.
pub fn default_roots() -> Vec<ClaudeSourceRoot> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let mut out: Vec<ClaudeSourceRoot> = Vec::new();
    let mut push = |label: &str, p: PathBuf| {
        out.push(ClaudeSourceRoot {
            label: label.to_string(),
            exists: p.exists(),
            path: p.display().to_string(),
            skill_count: 0,
        });
    };
    push("user", home.join(".claude").join("skills"));
    push("user-agents", home.join(".claude").join("agents"));
    push("plugins-marketplaces", home.join(".claude").join("plugins").join("marketplaces"));
    push("plugins-cache", home.join(".claude").join("plugins").join("cache"));
    // Common sibling repos people clone next to their home dir.
    push("repo:claude-skills", home.join("claude-skills"));
    out
}

/// Walk a root looking for Claude skill files. The Claude convention is
/// `<dir>/SKILL.md`, but plugin marketplaces also ship plain `*.md` files
/// with frontmatter — we accept both. Symlinks are followed but bounded by
/// max recursion depth so cycles can't deadlock us.
fn scan_root(root: &Path, out: &mut Vec<PathBuf>) {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
        if depth > 8 {
            return;
        }
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
            // Resolve symlinks once per entry.
            let resolved = if meta.file_type().is_symlink() {
                match std::fs::canonicalize(&path) {
                    Ok(p) => p,
                    Err(_) => continue,
                }
            } else {
                path.clone()
            };
            let resolved_meta = match std::fs::metadata(&resolved) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if resolved_meta.is_dir() {
                walk(&resolved, out, depth + 1);
            } else if resolved_meta.is_file() {
                let name = resolved.file_name().and_then(|s| s.to_str()).unwrap_or("");
                let lower = name.to_ascii_lowercase();
                if lower == "skill.md" || (lower.ends_with(".md") && depth >= 1) {
                    out.push(resolved);
                }
            }
        }
    }
    walk(root, out, 0);
}

fn import_one(src: &Path, dest_dir: &Path) -> Result<ImportedSkill> {
    let raw = std::fs::read_to_string(src)
        .map_err(|e| Error::Other(format!("read {}: {}", src.display(), e)))?;
    let (fm_text, body) = match split_frontmatter(&raw) {
        Some(x) => x,
        None => {
            return Ok(ImportedSkill {
                id: String::new(),
                name: src
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("?")
                    .to_string(),
                source_path: src.display().to_string(),
                written_to: String::new(),
                status: ImportStatus::Skipped,
                warnings: vec!["no YAML frontmatter".into()],
            });
        }
    };
    let parsed = ClaudeFrontmatter::from_yaml_text(fm_text);
    let parsed = match parsed {
        Some(p) => p,
        None => {
            return Ok(ImportedSkill {
                id: String::new(),
                name: src
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("?")
                    .to_string(),
                source_path: src.display().to_string(),
                written_to: String::new(),
                status: ImportStatus::Skipped,
                warnings: vec!["frontmatter has no `name` field".into()],
            });
        }
    };

    let pigide = map_to_pigide(&parsed, src, body);
    let dest = dest_dir.join(format!("{}.md", pigide.frontmatter.id));
    let new_text = render_pigide_skill(&pigide);

    let prior = std::fs::read_to_string(&dest).ok();
    let status = match prior {
        Some(prev) if prev == new_text => ImportStatus::Unchanged,
        Some(_) => {
            std::fs::write(&dest, &new_text)?;
            ImportStatus::Updated
        }
        None => {
            std::fs::write(&dest, &new_text)?;
            ImportStatus::Created
        }
    };

    Ok(ImportedSkill {
        id: pigide.frontmatter.id,
        name: pigide.frontmatter.name,
        source_path: src.display().to_string(),
        written_to: dest.display().to_string(),
        status,
        warnings: pigide.warnings,
    })
}

/// Subset of Claude's frontmatter that we actually consume.
#[derive(Debug, Clone, Default, Deserialize)]
struct ClaudeFrontmatter {
    name: String,
    #[serde(default)]
    description: String,
    /// Either `allowed-tools` (skills) or `tools` (agents).
    #[serde(default, alias = "tools", rename = "allowed-tools")]
    allowed_tools: Vec<String>,
    #[serde(default)]
    model: Option<String>,
}

impl ClaudeFrontmatter {
    /// Parse a Claude frontmatter blob. Tries `gray_matter` first (handles
    /// nested YAML, comments, multi-line values), falls back to a hand-rolled
    /// single-key reader for the small set of fields we care about.
    fn from_yaml_text(fm: &str) -> Option<Self> {
        use gray_matter::engine::YAML;
        use gray_matter::Matter;
        let wrapped = format!("---\n{}\n---\n", fm.trim_matches('\n'));
        let m = Matter::<YAML>::new();
        let p = m.parse(&wrapped);
        if let Some(data) = p.data {
            if let Ok(out) = data.deserialize::<ClaudeFrontmatter>() {
                if !out.name.trim().is_empty() {
                    return Some(out);
                }
            }
        }
        // Fallback: parse `name`, `description`, `allowed-tools` (block-list
        // form), `model` directly.
        let mut out = ClaudeFrontmatter::default();
        let mut in_tools_block = false;
        for raw_line in fm.lines() {
            let line = raw_line.trim_end();
            if in_tools_block {
                if let Some(item) = line.strip_prefix("  - ") {
                    out.allowed_tools.push(item.trim().trim_matches(&['"', '\''][..]).to_string());
                    continue;
                }
                if let Some(item) = line.strip_prefix("- ") {
                    out.allowed_tools.push(item.trim().trim_matches(&['"', '\''][..]).to_string());
                    continue;
                }
                in_tools_block = false;
            }
            let (k, v) = match line.split_once(':') {
                Some(x) => x,
                None => continue,
            };
            let key = k.trim();
            let val = v.trim();
            match key {
                "name" => out.name = val.trim_matches(&['"', '\''][..]).to_string(),
                "description" => {
                    out.description = val.trim_matches(&['"', '\''][..]).to_string()
                }
                "model" => {
                    if !val.is_empty() {
                        out.model =
                            Some(val.trim_matches(&['"', '\''][..]).to_string())
                    }
                }
                "allowed-tools" | "tools" => {
                    let trimmed = val.trim();
                    if trimmed.is_empty() {
                        in_tools_block = true;
                    } else if let Some(rest) = trimmed
                        .strip_prefix('[')
                        .and_then(|s| s.strip_suffix(']'))
                    {
                        for item in rest.split(',') {
                            let s = item.trim().trim_matches(&['"', '\''][..]);
                            if !s.is_empty() {
                                out.allowed_tools.push(s.to_string());
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        if out.name.trim().is_empty() {
            return None;
        }
        Some(out)
    }
}

/// Internal staging type before we render to disk.
struct PigideSkill {
    frontmatter: SkillFrontmatter,
    body: String,
    warnings: Vec<String>,
}

fn map_to_pigide(c: &ClaudeFrontmatter, src: &Path, body: &str) -> PigideSkill {
    let id = make_pigide_id(&c.name);
    let mut tags: Vec<String> = vec!["imported".into(), "claude".into()];
    let mut warnings: Vec<String> = Vec::new();
    for t in &c.allowed_tools {
        let mapped = map_tool_name(t);
        if mapped.is_none() {
            warnings.push(format!("unmapped Claude tool: {}", t));
        }
        tags.push(format!("claude-tools:{}", t));
    }
    let fm = SkillFrontmatter {
        id: id.clone(),
        name: humanize_name(&c.name),
        description: c.description.clone(),
        version: 1,
        priority: 50,
        tags,
        triggers: Vec::new(),
        inputs: Vec::new(),
        outputs: Vec::new(),
        model_hint: c.model.clone(),
        enabled: true,
    };
    let cleaned_body = body.trim_start_matches('\n').to_string();
    let body_with_pointer = format!(
        "<!-- imported-from: {} -->\n<!-- claude-skill-name: {} -->\n{}",
        src.display(),
        c.name,
        cleaned_body
    );
    PigideSkill {
        frontmatter: fm,
        body: body_with_pointer,
        warnings,
    }
}

/// Best-effort mapping of Claude tool names to PigIDE-known tools. Returns
/// `None` for tools we don't recognize, so the caller can warn.
///
/// PigIDE's "tools" surface today is the set of Tauri commands the
/// orchestrator can hand the agent — not a 1:1 of Claude's set. We don't
/// enforce an allow-list at compose-time (the registry has no such concept
/// yet); we just record the original names as tags so a future enforcement
/// layer can act on them.
fn map_tool_name(claude_tool: &str) -> Option<&'static str> {
    let lower = claude_tool.to_ascii_lowercase();
    let stripped = lower.split('(').next().unwrap_or(&lower).trim();
    match stripped {
        "read" => Some("read"),
        "write" => Some("write"),
        "edit" => Some("edit"),
        "bash" => Some("bash"),
        "grep" => Some("grep"),
        "glob" => Some("glob"),
        "webfetch" | "web_fetch" => Some("web-fetch"),
        "websearch" | "web_search" => Some("web-search"),
        "agent" | "task" => Some("agent"),
        "todowrite" | "task_write" => Some("tasks"),
        "askuserquestion" | "ask_user_question" => Some("ask-user"),
        _ => None,
    }
}

/// Render the imported skill back to a `.md` with PigIDE frontmatter.
fn render_pigide_skill(s: &PigideSkill) -> String {
    let mut out = String::with_capacity(512 + s.body.len());
    out.push_str("---\n");
    out.push_str(&format!("id: {}\n", s.frontmatter.id));
    out.push_str(&format!(
        "name: {}\n",
        yaml_string(&s.frontmatter.name)
    ));
    out.push_str(&format!(
        "description: {}\n",
        yaml_string(&s.frontmatter.description)
    ));
    out.push_str(&format!("version: {}\n", s.frontmatter.version));
    out.push_str(&format!("priority: {}\n", s.frontmatter.priority));
    if !s.frontmatter.tags.is_empty() {
        out.push_str("tags:\n");
        for t in &s.frontmatter.tags {
            out.push_str(&format!("  - {}\n", yaml_string(t)));
        }
    } else {
        out.push_str("tags: []\n");
    }
    out.push_str("triggers: []\n");
    if let Some(ref m) = s.frontmatter.model_hint {
        out.push_str(&format!("model_hint: {}\n", yaml_string(m)));
    }
    out.push_str(&format!(
        "enabled: {}\n",
        if s.frontmatter.enabled { "true" } else { "false" }
    ));
    out.push_str("---\n");
    out.push_str(&s.body);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Quote a YAML scalar when it contains characters that would otherwise be
/// ambiguous (colons, quotes, hashes, leading whitespace). For simple
/// strings we emit them bare to keep the file diff-friendly.
fn yaml_string(s: &str) -> String {
    let needs_quote = s.is_empty()
        || s.starts_with(' ')
        || s.ends_with(' ')
        || s.contains(':')
        || s.contains('#')
        || s.contains('"')
        || s.contains('\'')
        || s.contains('\n')
        || s.contains('\\');
    if !needs_quote {
        return s.to_string();
    }
    let escaped: String = s
        .chars()
        .flat_map(|c| match c {
            '\\' => vec!['\\', '\\'],
            '"' => vec!['\\', '"'],
            '\n' => vec!['\\', 'n'],
            other => vec![other],
        })
        .collect();
    format!("\"{}\"", escaped)
}

/// Slugify a Claude skill name to a PigIDE id matching `^[a-z0-9][a-z0-9-]{1,63}$`.
/// Always prefixed with `claude-` so imported skills can't collide with
/// hand-authored PigIDE ids.
fn make_pigide_id(name: &str) -> String {
    let mut s = String::new();
    let mut prev_dash = false;
    for c in name.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            s.push(c);
            prev_dash = false;
        } else if !prev_dash && !s.is_empty() {
            s.push('-');
            prev_dash = true;
        }
    }
    while s.ends_with('-') {
        s.pop();
    }
    if s.is_empty() {
        s.push_str("skill");
    }
    let id = if s.starts_with("claude-") {
        s
    } else {
        format!("claude-{}", s)
    };
    if id.len() > 64 {
        id[..64].trim_end_matches('-').to_string()
    } else {
        id
    }
}

fn humanize_name(name: &str) -> String {
    if name.is_empty() {
        return name.to_string();
    }
    name.split(|c: char| c == '-' || c == '_' || c == '/')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut chars = p.chars();
            match chars.next() {
                Some(c) => c.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

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
            path.push(format!("pigide-claude-import-{}-{}", tag, nanos));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    const CLAUDE_SAMPLE: &str = r#"---
name: gsd-debug
description: "Systematic debugging with persistent state"
allowed-tools:
  - Read
  - Write
  - Bash
  - Agent
  - PrivateInternalTool
---

# GSD Debug

Use when debugging.
"#;

    fn write(p: &Path, body: &str) {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, body).unwrap();
    }

    #[test]
    fn parser_extracts_required_fields() {
        let fm = ClaudeFrontmatter::from_yaml_text(
            "name: gsd-debug\ndescription: \"d\"\nallowed-tools:\n  - Read\n  - Bash\n",
        )
        .unwrap();
        assert_eq!(fm.name, "gsd-debug");
        assert_eq!(fm.description, "d");
        assert_eq!(fm.allowed_tools, vec!["Read", "Bash"]);
    }

    #[test]
    fn parser_handles_inline_tools_list() {
        let fm = ClaudeFrontmatter::from_yaml_text(
            "name: x\ndescription: \"d\"\nallowed-tools: [Read, Bash]\n",
        )
        .unwrap();
        assert_eq!(fm.allowed_tools, vec!["Read", "Bash"]);
    }

    #[test]
    fn parser_returns_none_when_no_name() {
        assert!(ClaudeFrontmatter::from_yaml_text("description: x\n").is_none());
    }

    #[test]
    fn id_slugification() {
        assert_eq!(make_pigide_id("gsd-debug"), "claude-gsd-debug");
        assert_eq!(make_pigide_id("Hello World"), "claude-hello-world");
        assert_eq!(make_pigide_id("foo/bar baz"), "claude-foo-bar-baz");
        // Already prefixed → no double-prefix.
        assert_eq!(make_pigide_id("claude-foo"), "claude-foo");
        // Always within 64 chars.
        assert!(make_pigide_id(&"a".repeat(200)).len() <= 64);
    }

    #[test]
    fn tool_mapping_known_and_unknown() {
        assert_eq!(map_tool_name("Read"), Some("read"));
        assert_eq!(map_tool_name("Bash(git*)"), Some("bash"));
        assert_eq!(map_tool_name("UnknownTool"), None);
    }

    #[test]
    fn import_creates_then_unchanged_then_updates() {
        // Create a fake source dir with one Claude skill.
        let src = Tmp::new("src");
        write(&src.path.join("gsd-debug").join("SKILL.md"), CLAUDE_SAMPLE);

        // Redirect HOME so `imported_dir()` lands in our tmp.
        let home = Tmp::new("home");
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &home.path);

        // First import.
        let r1 = import(&[src.path.clone()]).unwrap();
        let row1 = r1
            .imported
            .iter()
            .find(|i| i.id == "claude-gsd-debug")
            .expect("expected gsd-debug to be imported");
        assert_eq!(row1.status, ImportStatus::Created);
        assert!(row1.warnings.iter().any(|w| w.contains("PrivateInternalTool")));

        // Second import — same content → unchanged.
        let r2 = import(&[src.path.clone()]).unwrap();
        let row2 = r2
            .imported
            .iter()
            .find(|i| i.id == "claude-gsd-debug")
            .unwrap();
        assert_eq!(row2.status, ImportStatus::Unchanged);
        // Same destination path, no duplication.
        assert_eq!(row1.written_to, row2.written_to);

        // Mutate source → updated.
        let mutated = CLAUDE_SAMPLE.replace("Systematic debugging", "Systematic debugging v2");
        write(&src.path.join("gsd-debug").join("SKILL.md"), &mutated);
        let r3 = import(&[src.path.clone()]).unwrap();
        let row3 = r3
            .imported
            .iter()
            .find(|i| i.id == "claude-gsd-debug")
            .unwrap();
        assert_eq!(row3.status, ImportStatus::Updated);

        // Restore HOME.
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn imported_file_is_valid_pigide_skill() {
        // End-to-end: import, then re-parse via the PigIDE parser.
        let src = Tmp::new("src2");
        write(&src.path.join("hello").join("SKILL.md"), CLAUDE_SAMPLE);
        let home = Tmp::new("home2");
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &home.path);

        let report = import(&[src.path.clone()]).unwrap();
        let row = &report.imported[0];
        let raw = std::fs::read_to_string(&row.written_to).unwrap();
        let parsed = crate::skills::skill::parse(
            &row.written_to,
            crate::skills::skill::SkillSourceTag::User,
            &raw,
        )
        .expect("PigIDE parse must succeed")
        .expect("must produce a skill");
        assert_eq!(parsed.id, "claude-gsd-debug");
        assert!(parsed.frontmatter.tags.iter().any(|t| t == "imported"));
        assert!(parsed.frontmatter.tags.iter().any(|t| t == "claude"));
        assert!(parsed.body.contains("imported-from"));

        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn skips_files_without_frontmatter() {
        let src = Tmp::new("src3");
        write(&src.path.join("misc").join("notes.md"), "no frontmatter here\n");
        let home = Tmp::new("home3");
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &home.path);

        let report = import(&[src.path.clone()]).unwrap();
        // Either the file is skipped or not picked up at all; the imported
        // count for files without `name:` must be zero.
        assert_eq!(
            report
                .imported
                .iter()
                .filter(|i| i.status == ImportStatus::Created
                    || i.status == ImportStatus::Updated)
                .count(),
            0
        );

        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }
}
