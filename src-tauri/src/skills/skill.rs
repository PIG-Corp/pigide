//! Skill model + frontmatter parser.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};

/// One loaded skill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    /// Stable id from frontmatter (`^[a-z0-9][a-z0-9-]{1,63}$`).
    pub id: String,
    /// Source root the file came from.
    pub source: SkillSourceTag,
    /// Absolute path on disk.
    pub path: String,
    /// Parsed frontmatter.
    pub frontmatter: SkillFrontmatter,
    /// Raw template body (handlebars-lite).
    pub body: String,
    /// Sha256 of frontmatter+body for change detection (hex).
    pub digest: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum SkillSourceTag {
    Builtin,
    User,
    Workspace,
}

impl SkillSourceTag {
    pub fn precedence(self) -> u8 {
        match self {
            SkillSourceTag::Workspace => 3,
            SkillSourceTag::User => 2,
            SkillSourceTag::Builtin => 1,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            SkillSourceTag::Workspace => "workspace",
            SkillSourceTag::User => "user",
            SkillSourceTag::Builtin => "built-in",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkillFrontmatter {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default = "default_priority")]
    pub priority: u32,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub triggers: Vec<String>,
    #[serde(default)]
    pub inputs: Vec<SkillInput>,
    #[serde(default)]
    pub outputs: Vec<SkillOutput>,
    #[serde(default)]
    pub model_hint: Option<String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_version() -> u32 {
    1
}
fn default_priority() -> u32 {
    50
}
fn default_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkillInput {
    pub name: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkillOutput {
    pub name: String,
    #[serde(default)]
    pub description: String,
}

/// Parse a markdown file with YAML frontmatter into a [`Skill`].
///
/// Returns `Ok(None)` when the file has no frontmatter (treated as not a
/// skill) and `Err(...)` only on hard schema violations.
pub fn parse(path: &str, source: SkillSourceTag, raw: &str) -> Result<Option<Skill>> {
    let (fm_text, body) = match split_frontmatter(raw) {
        Some(x) => x,
        None => return Ok(None),
    };
    let fm: SkillFrontmatter = parse_frontmatter(fm_text)?;
    validate(&fm, body)?;
    let mut digest = sha256_hex(raw);
    digest.truncate(16);
    Ok(Some(Skill {
        id: fm.id.clone(),
        source,
        path: path.to_string(),
        frontmatter: fm,
        body: body.to_string(),
        digest,
    }))
}

/// Parse the YAML frontmatter into a typed struct using `gray_matter` for
/// permissive YAML, falling back to a hand-rolled parser if the dependency
/// returns nothing useful (the project's gray_matter feature set is YAML-only
/// and reasonably complete).
fn parse_frontmatter(fm: &str) -> Result<SkillFrontmatter> {
    use gray_matter::engine::YAML;
    use gray_matter::Matter;

    let wrapped = format!("---\n{}\n---\n", fm.trim_matches('\n'));
    let matter = Matter::<YAML>::new();
    let parsed = matter.parse(&wrapped);
    if let Some(data) = parsed.data {
        if let Ok(out) = data.deserialize::<SkillFrontmatter>() {
            if !out.id.is_empty() {
                return Ok(out);
            }
        }
    }

    // Fallback: extract the small set of keys we strictly need.
    let mut out = SkillFrontmatter {
        version: default_version(),
        priority: default_priority(),
        enabled: default_enabled(),
        ..Default::default()
    };
    for raw_line in fm.lines() {
        let line = raw_line.trim_end();
        let (k, v) = match line.split_once(':') {
            Some(x) => x,
            None => continue,
        };
        let key = k.trim();
        let val = v.trim();
        match key {
            "id" => out.id = val.trim_matches(&['"', '\''][..]).to_string(),
            "name" => out.name = val.trim_matches(&['"', '\''][..]).to_string(),
            "description" => {
                out.description = val.trim_matches(&['"', '\''][..]).to_string()
            }
            "version" => {
                if let Ok(n) = val.parse() {
                    out.version = n;
                }
            }
            "priority" => {
                if let Ok(n) = val.parse() {
                    out.priority = n;
                }
            }
            "tags" => out.tags = parse_inline_list(val),
            "triggers" => out.triggers = parse_inline_list(val),
            "model_hint" => {
                out.model_hint = Some(val.trim_matches(&['"', '\''][..]).to_string())
            }
            "enabled" => out.enabled = !matches!(val, "false" | "no" | "0"),
            _ => {}
        }
    }
    Ok(out)
}

fn parse_inline_list(v: &str) -> Vec<String> {
    let trimmed = v.trim();
    let inner = trimmed
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(trimmed);
    inner
        .split(',')
        .map(|s| s.trim().trim_matches(&['\'', '"'][..]).to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Split a markdown file with leading `---\n...\n---\n` frontmatter.
/// Returns `(frontmatter_inner, body)` or `None` if the file has no
/// frontmatter.
pub fn split_frontmatter(raw: &str) -> Option<(&str, &str)> {
    let stripped = raw
        .strip_prefix("---\n")
        .or_else(|| raw.strip_prefix("---\r\n"))?;
    let mut consumed = 0usize;
    for line in stripped.split_inclusive('\n') {
        if line.trim_end() == "---" {
            let fm = &stripped[..consumed];
            let body_start = consumed + line.len();
            let body = &stripped[body_start..];
            return Some((fm, body));
        }
        consumed += line.len();
    }
    None
}

/// Validate a parsed frontmatter + body. Returns Err on hard violations.
pub fn validate(fm: &SkillFrontmatter, body: &str) -> Result<()> {
    let id = fm.id.trim();
    if id.is_empty() {
        return Err(Error::Invalid("skill: id is required".into()));
    }
    if !valid_id(id) {
        return Err(Error::Invalid(format!(
            "skill: invalid id {:?} (expected ^[a-z0-9][a-z0-9-]{{1,63}}$)",
            id
        )));
    }
    if fm.name.trim().is_empty() {
        return Err(Error::Invalid("skill: name is required".into()));
    }
    if body.trim().is_empty() {
        return Err(Error::Invalid("skill: body is empty".into()));
    }
    if body.len() > 32 * 1024 {
        return Err(Error::Invalid("skill: body > 32 KB".into()));
    }
    if fm.priority > 100 {
        return Err(Error::Invalid("skill: priority must be 0..100".into()));
    }
    Ok(())
}

fn valid_id(id: &str) -> bool {
    if id.len() < 2 || id.len() > 64 {
        return false;
    }
    let mut chars = id.chars();
    let first = chars.next().unwrap();
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn sha256_hex(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    let out = h.finalize();
    let mut hex = String::with_capacity(out.len() * 2);
    for b in out.iter() {
        hex.push_str(&format!("{:02x}", b));
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"---
id: hello-world
name: Hello World
description: Greets you
priority: 70
tags: [greet, hi]
triggers: [hello, "привет"]
enabled: true
---
Hello {{name}}!
"#;

    #[test]
    fn parses_minimal() {
        let s = parse("/x.md", SkillSourceTag::Builtin, SAMPLE)
            .unwrap()
            .unwrap();
        assert_eq!(s.id, "hello-world");
        assert_eq!(s.frontmatter.priority, 70);
        assert_eq!(s.frontmatter.tags, vec!["greet", "hi"]);
        assert!(s.body.contains("Hello"));
    }

    #[test]
    fn rejects_bad_id() {
        let raw = "---\nid: BadID\nname: x\n---\nbody\n";
        let err = parse("/x.md", SkillSourceTag::Builtin, raw).unwrap_err();
        match err {
            Error::Invalid(msg) => assert!(msg.contains("invalid id")),
            other => panic!("unexpected: {:?}", other),
        }
    }

    #[test]
    fn no_frontmatter_returns_none() {
        let raw = "just text without yaml\n";
        assert!(parse("/x.md", SkillSourceTag::Builtin, raw)
            .unwrap()
            .is_none());
    }

    #[test]
    fn rejects_empty_body() {
        let raw = "---\nid: foo\nname: Foo\n---\n   \n";
        let err = parse("/x.md", SkillSourceTag::Builtin, raw).unwrap_err();
        assert!(matches!(err, Error::Invalid(_)));
    }
}
