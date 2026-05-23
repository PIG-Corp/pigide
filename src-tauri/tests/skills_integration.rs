//! Integration test for the Skills system.
//!
//! Spins up an isolated registry rooted at a tmp dir with three skills,
//! routes a fake user message through it, composes a system prompt, and
//! asserts the right skills end up in the prompt.

use pigide_lib::skills::compose_system_prompt;
use pigide_lib::skills::registry::{SkillRegistry, SkillSource};
use pigide_lib::skills::router::{route, RouterConfig, RouterMode};
use pigide_lib::skills::skill::SkillSourceTag;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

struct Tmp(PathBuf);
impl Tmp {
    fn new(tag: &str) -> Self {
        let mut p = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        p.push(format!("pigide-skills-itest-{}-{}", tag, nanos));
        fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
impl Tmp {
    fn path(&self) -> &Path {
        &self.0
    }
}

fn write(path: &Path, body: &str) {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p).unwrap();
    }
    let mut f = fs::File::create(path).unwrap();
    f.write_all(body.as_bytes()).unwrap();
}

fn skill_md(id: &str, name: &str, prio: u32, triggers: &[&str]) -> String {
    let trig = triggers
        .iter()
        .map(|t| format!("\"{}\"", t))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "---\nid: {id}\nname: {name}\npriority: {prio}\ntriggers: [{trig}]\ndescription: it tests {id}\n---\n[BODY of {name}] hi {{{{user_message}}}}\n",
        id = id,
        name = name,
        prio = prio,
        trig = trig,
    )
}

#[test]
fn end_to_end_compose_with_routing() {
    let td = Tmp::new("e2e");
    write(
        &td.path().join("alpha.md"),
        &skill_md("alpha", "Alpha", 70, &["foo"]),
    );
    write(
        &td.path().join("beta.md"),
        &skill_md("beta", "Beta", 50, &["bar"]),
    );
    write(
        &td.path().join("gamma.md"),
        &skill_md("gamma", "Gamma", 50, &["baz"]),
    );

    let reg = SkillRegistry::new();
    reg.set_sources(vec![SkillSource {
        tag: SkillSourceTag::Builtin,
        root: td.path().to_path_buf(),
    }]);
    reg.reload_all().unwrap();
    let active = reg.active();
    assert_eq!(active.len(), 3);

    let cfg = RouterConfig {
        mode: RouterMode::Deterministic,
        ..Default::default()
    };
    let routed = route(&active, "please foo this and bar that", &cfg, false);
    let ids: Vec<_> = routed.selected.iter().map(|s| s.id.clone()).collect();
    assert!(ids.contains(&"alpha".into()), "alpha missing in {:?}", ids);
    assert!(ids.contains(&"beta".into()), "beta missing in {:?}", ids);
    assert!(!ids.contains(&"gamma".into()), "gamma should not match");

    // Compose: ordered = active in routed order.
    let by_id: std::collections::HashMap<_, _> = active.iter().map(|s| (s.id.clone(), s)).collect();
    let ordered: Vec<_> = routed
        .selected
        .iter()
        .filter_map(|s| by_id.get(&s.id).copied())
        .collect();
    let mut ctx = BTreeMap::new();
    ctx.insert(
        "user_message".into(),
        Value::String("please foo this and bar that".into()),
    );
    let res = compose_system_prompt("BASE PROMPT", &ordered, &ctx, 8000);
    assert!(res.prompt.contains("[ACTIVE SKILLS]"));
    assert!(res.prompt.contains("[SKILL: Alpha"));
    assert!(res.prompt.contains("[SKILL: Beta"));
    assert!(!res.prompt.contains("[SKILL: Gamma"));
    assert!(res.prompt.contains("hi please foo this"));
}

#[test]
fn hot_reload_picks_up_new_file() {
    let td = Tmp::new("hot");
    let reg = SkillRegistry::new();
    reg.set_sources(vec![SkillSource {
        tag: SkillSourceTag::Builtin,
        root: td.path().to_path_buf(),
    }]);
    reg.reload_all().unwrap();
    assert!(reg.active().is_empty());

    let p = td.path().join("late.md");
    write(&p, &skill_md("late", "Late", 50, &["kick"]));
    reg.reload_path(&p).unwrap();
    let active = reg.active();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].id, "late");

    // Edit + re-load.
    write(&p, &skill_md("late", "Late v2", 50, &["kick"]));
    reg.reload_path(&p).unwrap();
    let active = reg.active();
    assert_eq!(active[0].frontmatter.name, "Late v2");

    // Delete + re-load.
    fs::remove_file(&p).unwrap();
    reg.reload_path(&p).unwrap();
    assert!(reg.active().is_empty());
}
