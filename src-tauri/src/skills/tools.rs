//! Tauri commands for the Skills system.
//!
//! Frontend ↔ backend surface for the Skills UI panel. Mirrors the
//! `*_skill*` commands in `commands.rs` and is wired into the
//! `invoke_handler` there.

use crate::db::{self, DbPool};
use crate::error::Result;
use crate::skills::registry::{SkillEntry, SkillRegistry};
use crate::skills::trace::TraceRow;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;

/// Public view of one skill for the UI (lightweight; full body lives behind
/// `get_skill`).
#[derive(Debug, Clone, Serialize)]
pub struct SkillView {
    pub id: String,
    pub name: String,
    pub description: String,
    pub source: String,
    pub path: String,
    pub priority: u32,
    pub tags: Vec<String>,
    pub triggers: Vec<String>,
    pub enabled: bool,
    pub override_disabled: bool,
    pub shadowed_by: Option<String>,
    pub digest: String,
}

impl From<&SkillEntry> for SkillView {
    fn from(e: &SkillEntry) -> Self {
        Self {
            id: e.skill.id.clone(),
            name: e.skill.frontmatter.name.clone(),
            description: e.skill.frontmatter.description.clone(),
            source: e.skill.source.as_str().into(),
            path: e.skill.path.clone(),
            priority: e.skill.frontmatter.priority,
            tags: e.skill.frontmatter.tags.clone(),
            triggers: e.skill.frontmatter.triggers.clone(),
            enabled: e.skill.frontmatter.enabled,
            override_disabled: e.override_disabled,
            shadowed_by: e.shadowed_by.map(|s| s.as_str().into()),
            digest: e.skill.digest.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillFull {
    #[serde(flatten)]
    pub view: SkillView,
    pub body: String,
}

/// Load `skills.disabled.<id>` overrides from `settings`.
pub fn load_overrides(pool: &DbPool) -> Result<HashMap<String, bool>> {
    let conn = pool.get()?;
    let mut stmt = conn.prepare(
        "SELECT key, value FROM settings WHERE key LIKE 'skills.disabled.%'",
    )?;
    let mut out = HashMap::new();
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    for row in rows.flatten() {
        let id = row.0.trim_start_matches("skills.disabled.").to_string();
        out.insert(id, row.1.eq_ignore_ascii_case("true"));
    }
    Ok(out)
}

pub fn list(reg: &SkillRegistry) -> Vec<SkillView> {
    reg.entries().iter().map(SkillView::from).collect()
}

pub fn get(reg: &SkillRegistry, id: &str) -> Option<SkillFull> {
    let entries = reg.entries();
    let entry = entries
        .iter()
        .find(|e| e.shadowed_by.is_none() && e.skill.id == id)?;
    Some(SkillFull {
        view: SkillView::from(entry),
        body: entry.skill.body.clone(),
    })
}

pub fn set_enabled(pool: &DbPool, reg: &SkillRegistry, id: &str, enabled: bool) -> Result<()> {
    let key = format!("skills.disabled.{}", id);
    db::set_setting(pool, &key, if enabled { "false" } else { "true" })?;
    let overrides = load_overrides(pool)?;
    reg.set_overrides(overrides);
    Ok(())
}

pub fn last_trace(pool: &DbPool, session_id: Option<&str>) -> Result<Option<TraceRow>> {
    crate::skills::trace::latest(pool, session_id)
}

/// Helper for `create_user_skill`: writes a stub to `~/.pigide/skills/<id>.md`
/// and returns the absolute path.
pub fn create_user_stub(id: &str, name: &str) -> Result<String> {
    use std::io::Write;
    let dir = crate::skills::registry::default_user_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.md", id));
    if path.exists() {
        return Err(crate::error::Error::Invalid(format!(
            "skill {} already exists at {}",
            id,
            path.display()
        )));
    }
    let body = format!(
        "---\nid: {id}\nname: {name}\ndescription: TODO — describe when this skill should fire\npriority: 50\ntags: []\ntriggers: []\nenabled: true\n---\n[SKILL — {name}]\n\nWrite the prompt body here. Use {{{{var}}}} for variables.\n",
        id = id,
        name = name,
    );
    let mut f = std::fs::File::create(&path)?;
    f.write_all(body.as_bytes())?;
    Ok(path.display().to_string())
}

#[allow(dead_code)]
fn _arc_use(_: Arc<SkillRegistry>) {}
