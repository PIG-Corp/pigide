//! Note model + YAML frontmatter (de)serialization.

use crate::error::{Error, Result};
use crate::memory::folders::Kind;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Where this note came from. `None` for user-created notes; `Some(...)`
/// for anything written by the ingest pipeline.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct IngestRecord {
    pub source_kind: String,
    #[serde(default)]
    pub source_ref: Option<String>,
    pub ingested_at: String,
    #[serde(default)]
    pub smart_pass_at: Option<String>,
}

/// In-memory representation of a note. The `path` field is workspace-relative
/// (or absolute, depending on caller); the frontmatter holds the canonical id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Note {
    pub id: String,
    pub slug: String,
    pub title: String,
    #[serde(default = "Kind::default_for_legacy")]
    pub kind: Kind,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub body: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub ingest: Option<IngestRecord>,
}

impl Note {
    pub fn new(slug: String, title: String, body: String) -> Self {
        let ts = Utc::now().to_rfc3339();
        Self {
            id: Uuid::new_v4().to_string(),
            slug,
            title,
            kind: Kind::default_for_legacy(),
            tags: Vec::new(),
            aliases: Vec::new(),
            body,
            created_at: ts.clone(),
            updated_at: ts,
            ingest: None,
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[allow(dead_code)] // kept for forward-compat with full-YAML parsing
struct Frontmatter {
    #[serde(default)]
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    created_at: String,
    #[serde(default)]
    updated_at: String,
}

/// Serialize a note to a markdown string with YAML frontmatter.
pub fn serialize(note: &Note) -> String {
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("id: {}\n", note.id));
    out.push_str(&format!("title: {}\n", yaml_escape(&note.title)));
    out.push_str(&format!("kind: {}\n", note.kind.as_str()));
    if !note.tags.is_empty() {
        out.push_str(&format!("tags: [{}]\n", note.tags.join(", ")));
    }
    if !note.aliases.is_empty() {
        out.push_str(&format!(
            "aliases: [{}]\n",
            note.aliases
                .iter()
                .map(|a| yaml_escape(a))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    out.push_str(&format!("created_at: {}\n", note.created_at));
    out.push_str(&format!("updated_at: {}\n", note.updated_at));
    if let Some(ing) = &note.ingest {
        out.push_str("ingest:\n");
        out.push_str(&format!("  source_kind: {}\n", ing.source_kind));
        if let Some(r) = &ing.source_ref {
            out.push_str(&format!("  source_ref: {}\n", yaml_escape(r)));
        }
        out.push_str(&format!("  ingested_at: {}\n", ing.ingested_at));
        if let Some(s) = &ing.smart_pass_at {
            out.push_str(&format!("  smart_pass_at: {}\n", s));
        }
    }
    out.push_str("---\n");
    out.push_str(&note.body);
    if !note.body.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Parse a markdown file string into a Note. Slug must be supplied by the
/// caller (derived from path).
pub fn parse(slug: &str, raw: &str) -> Result<Note> {
    // Match both `---\n...---\n` and `---\r\n...---\r\n`.
    let (fm, body) = split_frontmatter(raw);
    let mut note = match fm {
        Some(fm_text) => parse_frontmatter(slug, fm_text, body)?,
        None => Note::new(slug.to_string(), slug_to_title(slug), raw.to_string()),
    };
    if note.title.trim().is_empty() {
        note.title = slug_to_title(slug);
    }
    Ok(note)
}

fn parse_frontmatter(slug: &str, fm: &str, body: &str) -> Result<Note> {
    // Hand-rolled tiny YAML reader, extended to recognise a one-level
    // nested `ingest:` block of the form
    //
    //   ingest:
    //     source_kind: task
    //     source_ref: abc-123
    //     ingested_at: 2026-...
    //     smart_pass_at: 2026-...
    let mut id = String::new();
    let mut title = String::new();
    let mut kind = Kind::default_for_legacy();
    let mut tags = Vec::new();
    let mut aliases = Vec::new();
    let mut created_at = String::new();
    let mut updated_at = String::new();
    let mut in_ingest = false;
    let mut ing_source_kind = String::new();
    let mut ing_source_ref: Option<String> = None;
    let mut ing_ingested_at = String::new();
    let mut ing_smart_pass_at: Option<String> = None;
    let mut ingest_seen = false;
    for raw_line in fm.lines() {
        let line = raw_line.trim_end();
        if line.is_empty() {
            continue;
        }
        let starts_indented = line.starts_with("  ") || line.starts_with('\t');
        if in_ingest && starts_indented {
            let trimmed = line.trim_start();
            let (k, v) = match trimmed.split_once(':') {
                Some(x) => x,
                None => continue,
            };
            let val = v.trim();
            match k.trim() {
                "source_kind" => ing_source_kind = yaml_unescape(val),
                "source_ref" => {
                    if !val.is_empty() {
                        ing_source_ref = Some(yaml_unescape(val));
                    }
                }
                "ingested_at" => ing_ingested_at = val.to_string(),
                "smart_pass_at" => {
                    if !val.is_empty() {
                        ing_smart_pass_at = Some(val.to_string());
                    }
                }
                _ => {}
            }
            continue;
        }
        in_ingest = false;
        let (k, v) = match line.split_once(':') {
            Some(x) => x,
            None => continue,
        };
        let key = k.trim();
        let val = v.trim();
        match key {
            "id" => id = val.to_string(),
            "title" => title = yaml_unescape(val),
            "kind" => {
                if let Some(parsed) = Kind::parse(val) {
                    kind = parsed;
                }
            }
            "tags" => tags = parse_inline_list(val),
            "aliases" => {
                aliases = parse_inline_list(val)
                    .into_iter()
                    .map(|s| yaml_unescape(&s))
                    .collect()
            }
            "created_at" => created_at = val.to_string(),
            "updated_at" => updated_at = val.to_string(),
            "ingest" => {
                in_ingest = true;
                ingest_seen = true;
            }
            _ => {}
        }
    }
    let id = if id.is_empty() {
        Uuid::new_v4().to_string()
    } else {
        id
    };
    let now = Utc::now().to_rfc3339();
    let ingest = if ingest_seen && !ing_source_kind.is_empty() {
        Some(IngestRecord {
            source_kind: ing_source_kind,
            source_ref: ing_source_ref,
            ingested_at: if ing_ingested_at.is_empty() {
                now.clone()
            } else {
                ing_ingested_at
            },
            smart_pass_at: ing_smart_pass_at,
        })
    } else {
        None
    };
    Ok(Note {
        id,
        slug: slug.to_string(),
        title,
        kind,
        tags,
        aliases,
        body: body.to_string(),
        created_at: if created_at.is_empty() {
            now.clone()
        } else {
            created_at
        },
        updated_at: if updated_at.is_empty() {
            now
        } else {
            updated_at
        },
        ingest,
    })
}

fn split_frontmatter(raw: &str) -> (Option<&str>, &str) {
    // Frontmatter is: starts with "---" line, followed by yaml, ends with "---" line.
    let stripped = raw
        .strip_prefix("---\n")
        .or_else(|| raw.strip_prefix("---\r\n"));
    let (fm_start, after_open) = match stripped {
        Some(rest) => (true, rest),
        None => (false, raw),
    };
    if !fm_start {
        return (None, raw);
    }
    // Find the closing "---" on its own line.
    let mut close_idx: Option<usize> = None;
    for (i, line) in after_open.lines().enumerate() {
        if line.trim_end() == "---" {
            close_idx = Some(i);
            break;
        }
    }
    let close = match close_idx {
        Some(c) => c,
        None => return (None, raw),
    };
    // Compute byte offsets up to that line and the line after.
    let mut consumed = 0usize;
    let mut fm_end = 0usize;
    let mut body_start = after_open.len();
    for (i, line) in after_open.split_inclusive('\n').enumerate() {
        if i == close {
            fm_end = consumed;
            body_start = consumed + line.len();
            break;
        }
        consumed += line.len();
    }
    let fm = &after_open[..fm_end];
    let body = &after_open[body_start..];
    (Some(fm), body)
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

fn yaml_escape(s: &str) -> String {
    if s.contains(':') || s.starts_with(' ') || s.ends_with(' ') {
        format!("\"{}\"", s.replace('"', "\\\""))
    } else {
        s.to_string()
    }
}
fn yaml_unescape(s: &str) -> String {
    let s = s.trim();
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        s[1..s.len() - 1].replace("\\\"", "\"")
    } else if s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2 {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

fn slug_to_title(slug: &str) -> String {
    let last = slug.rsplit('/').next().unwrap_or(slug);
    last.replace(['-', '_'], " ")
}

/// Used by the watcher path → re-parse from disk.
pub fn read(path: &std::path::Path) -> Result<String> {
    std::fs::read_to_string(path).map_err(|e| Error::Other(format!("read note: {}", e)))
}

pub fn write(path: &std::path::Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_with_frontmatter() {
        let mut n = Note::new(
            "auth-pattern".into(),
            "Auth pattern".into(),
            "Body line 1\nBody line 2\n".into(),
        );
        n.tags = vec!["auth".into(), "security".into()];
        n.aliases = vec!["authn".into()];
        let raw = serialize(&n);
        let parsed = parse("auth-pattern", &raw).unwrap();
        assert_eq!(parsed.title, "Auth pattern");
        assert_eq!(parsed.tags, vec!["auth", "security"]);
        assert_eq!(parsed.aliases, vec!["authn"]);
        assert!(parsed.body.starts_with("Body line 1"));
    }

    #[test]
    fn raw_markdown_without_frontmatter() {
        let n = parse("orphan", "Just text.\n").unwrap();
        assert_eq!(n.title, "orphan");
        assert_eq!(n.body, "Just text.\n");
    }

    #[test]
    fn round_trip_preserves_kind_and_ingest() {
        let mut n = Note::new(
            "tasks/abc-123".into(),
            "Task ABC".into(),
            "did the thing\n".into(),
        );
        n.kind = crate::memory::folders::Kind::Task;
        n.ingest = Some(IngestRecord {
            source_kind: "task".into(),
            source_ref: Some("abc-123".into()),
            ingested_at: "2026-05-27T15:00:00Z".into(),
            smart_pass_at: None,
        });
        let raw = serialize(&n);
        assert!(raw.contains("kind: task"));
        assert!(raw.contains("ingest:"));
        assert!(raw.contains("source_kind: task"));
        let parsed = parse("tasks/abc-123", &raw).unwrap();
        assert_eq!(parsed.kind, crate::memory::folders::Kind::Task);
        let ing = parsed.ingest.expect("ingest preserved");
        assert_eq!(ing.source_kind, "task");
        assert_eq!(ing.source_ref.as_deref(), Some("abc-123"));
        assert!(ing.smart_pass_at.is_none());
    }

    #[test]
    fn legacy_note_without_kind_defaults_to_source() {
        let raw = "---\nid: 11111111-1111-1111-1111-111111111111\ntitle: Old\ncreated_at: 2025-01-01T00:00:00Z\nupdated_at: 2025-01-01T00:00:00Z\n---\nbody\n";
        let n = parse("old-flat", raw).unwrap();
        assert_eq!(n.kind, crate::memory::folders::Kind::Source);
        assert!(n.ingest.is_none());
    }

    #[test]
    fn smart_pass_at_round_trips_when_set() {
        let mut n = Note::new("concepts/x".into(), "X".into(), "".into());
        n.kind = crate::memory::folders::Kind::Concept;
        n.ingest = Some(IngestRecord {
            source_kind: "task_complete".into(),
            source_ref: Some("t-1".into()),
            ingested_at: "2026-05-27T15:00:00Z".into(),
            smart_pass_at: Some("2026-05-27T15:05:00Z".into()),
        });
        let raw = serialize(&n);
        let parsed = parse("concepts/x", &raw).unwrap();
        assert_eq!(
            parsed.ingest.unwrap().smart_pass_at.as_deref(),
            Some("2026-05-27T15:05:00Z")
        );
    }
}
