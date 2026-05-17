//! Per-turn skill router.
//!
//! Pure / deterministic by default. The optional LLM tie-break is gated
//! behind [`RouterConfig::llm_fallback`] and never runs from unit tests.

use crate::skills::skill::Skill;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouterMode {
    /// Skills system disabled; router never returns anything.
    Off,
    /// Pure lexical/tag/trigger pass — the default.
    Deterministic,
    /// Deterministic first; LLM tie-break only if no deterministic hits.
    Auto,
}

impl Default for RouterMode {
    fn default() -> Self {
        RouterMode::Deterministic
    }
}

impl RouterMode {
    pub fn from_str(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "off" | "false" | "0" => RouterMode::Off,
            "auto" => RouterMode::Auto,
            _ => RouterMode::Deterministic,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            RouterMode::Off => "off",
            RouterMode::Deterministic => "deterministic",
            RouterMode::Auto => "auto",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RouterConfig {
    pub mode: RouterMode,
    pub max_skills: usize,
    pub token_budget: usize,
    pub llm_fallback: bool,
    /// Force-include these skill ids regardless of score (e.g. when the
    /// Architect is about to dispatch and we want UserSkillPromptEngineer
    /// in the active set).
    pub force_include: Vec<String>,
    /// User mention syntax to honour. Default: `@<id>` and `@skill:<id>`.
    pub mention_prefixes: Vec<String>,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            mode: RouterMode::default(),
            max_skills: 4,
            token_budget: 4000,
            llm_fallback: false,
            force_include: Vec::new(),
            mention_prefixes: vec!["@skill:".into(), "@".into()],
        }
    }
}

/// Result of a single routing pass.
#[derive(Debug, Clone)]
pub struct RouteResult {
    pub selected: Vec<Selection>,
    pub rejected: Vec<Selection>,
    pub fallback_used: bool,
}

#[derive(Debug, Clone)]
pub struct Selection {
    pub id: String,
    pub score: f32,
    pub reasons: Vec<String>,
}

/// Run the deterministic routing pass over `skills` for `message`.
///
/// `dispatching` is `true` when this turn is about to call `send_to_agent` —
/// we use it to promote the meta-skill `user-skill-prompt-engineer`.
pub fn route(
    skills: &[Skill],
    message: &str,
    cfg: &RouterConfig,
    dispatching: bool,
) -> RouteResult {
    if cfg.mode == RouterMode::Off {
        return RouteResult {
            selected: Vec::new(),
            rejected: Vec::new(),
            fallback_used: false,
        };
    }

    let lower = message.to_lowercase();
    let words = tokenize(&lower);

    let mut scored: Vec<Selection> = Vec::new();
    for sk in skills {
        if !sk.frontmatter.enabled {
            continue;
        }
        let mut score = 0.0_f32;
        let mut reasons = Vec::new();

        // 1) explicit mentions: @<id> or @skill:<id>
        for prefix in &cfg.mention_prefixes {
            let needle = format!("{}{}", prefix, sk.id);
            if lower.contains(&needle.to_lowercase()) {
                score += 100.0;
                reasons.push(format!("mention {}", needle));
                break;
            }
        }

        // 2) triggers (substrings)
        for trig in &sk.frontmatter.triggers {
            if trig.is_empty() {
                continue;
            }
            if lower.contains(&trig.to_lowercase()) {
                score += 5.0;
                reasons.push(format!("trigger '{}'", trig));
            }
        }

        // 3) tags ∩ words
        for tag in &sk.frontmatter.tags {
            if words.contains(&tag.to_lowercase()) {
                score += 2.0;
                reasons.push(format!("tag '{}'", tag));
            }
        }

        // 4) description tokens overlap (capped)
        let desc_tokens = tokenize(&sk.frontmatter.description.to_lowercase());
        let mut overlap = 0;
        for t in &desc_tokens {
            if t.len() < 4 {
                continue;
            }
            if words.contains(t) {
                overlap += 1;
            }
        }
        if overlap > 0 {
            let bonus = (overlap as f32).min(3.0);
            score += bonus;
            reasons.push(format!("desc-overlap {}", overlap));
        }

        // 5) priority continuous tiebreaker
        score += sk.frontmatter.priority as f32 / 100.0;

        // 6) dispatching promotion
        if dispatching && sk.id == "user-skill-prompt-engineer" {
            score += 50.0;
            reasons.push("dispatching → promoted".into());
        }

        // 7) explicit force-include from caller
        if cfg.force_include.iter().any(|f| f == &sk.id) {
            score += 200.0;
            reasons.push("force-include".into());
        }

        scored.push(Selection {
            id: sk.id.clone(),
            score,
            reasons,
        });
    }

    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let cutoff = 1.0_f32;
    let mut selected: Vec<Selection> = Vec::new();
    let mut rejected: Vec<Selection> = Vec::new();
    for s in scored.into_iter() {
        let keep = s.score >= cutoff && selected.len() < cfg.max_skills;
        if keep {
            selected.push(s);
        } else {
            rejected.push(s);
        }
    }

    // Truncate to deterministic max — the composer enforces the byte budget.
    selected.truncate(cfg.max_skills);

    RouteResult {
        selected,
        rejected,
        fallback_used: false,
    }
}

fn tokenize(s: &str) -> std::collections::HashSet<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::skill::{parse, SkillSourceTag};

    fn skill(id: &str, name: &str, body: &str, prio: u32, triggers: &[&str], tags: &[&str]) -> Skill {
        let trig = triggers
            .iter()
            .map(|t| format!("\"{}\"", t))
            .collect::<Vec<_>>()
            .join(", ");
        let tg = tags.join(", ");
        let raw = format!(
            "---\nid: {}\nname: {}\ndescription: test desc {}\npriority: {}\ntags: [{}]\ntriggers: [{}]\n---\n{}\n",
            id, name, id, prio, tg, trig, body
        );
        parse("/x.md", SkillSourceTag::Builtin, &raw).unwrap().unwrap()
    }

    #[test]
    fn picks_by_trigger() {
        let sks = vec![
            skill("alpha", "Alpha", "body", 50, &["foo"], &[]),
            skill("beta", "Beta", "body", 50, &["bar"], &[]),
        ];
        let r = route(&sks, "please foo this for me", &RouterConfig::default(), false);
        assert_eq!(r.selected.first().unwrap().id, "alpha");
    }

    #[test]
    fn explicit_mention_wins() {
        let sks = vec![
            skill("alpha", "Alpha", "body", 90, &["foo"], &[]),
            skill("beta", "Beta", "body", 50, &[], &[]),
        ];
        let r = route(
            &sks,
            "please use @skill:beta on this",
            &RouterConfig::default(),
            false,
        );
        assert_eq!(r.selected.first().unwrap().id, "beta");
    }

    #[test]
    fn off_returns_nothing() {
        let sks = vec![skill("alpha", "Alpha", "body", 99, &["match"], &[])];
        let cfg = RouterConfig {
            mode: RouterMode::Off,
            ..Default::default()
        };
        let r = route(&sks, "match match match", &cfg, false);
        assert!(r.selected.is_empty());
    }

    #[test]
    fn dispatching_promotes_meta_skill() {
        let sks = vec![
            skill("user-skill-prompt-engineer", "USPE", "body", 50, &[], &[]),
            skill("alpha", "Alpha", "body", 90, &["match"], &[]),
        ];
        let r = route(&sks, "match this please", &RouterConfig::default(), true);
        // Both should appear; USPE first because of the dispatching promo.
        assert_eq!(r.selected.first().unwrap().id, "user-skill-prompt-engineer");
    }

    #[test]
    fn cutoff_drops_irrelevant() {
        let sks = vec![skill("alpha", "Alpha", "body", 0, &[], &[])];
        let r = route(&sks, "totally unrelated message", &RouterConfig::default(), false);
        assert!(r.selected.is_empty());
        assert_eq!(r.rejected.len(), 1);
    }

    #[test]
    fn respects_max_skills() {
        let sks = (0..6)
            .map(|i| {
                skill(
                    &format!("s{}", i),
                    &format!("S{}", i),
                    "body",
                    50,
                    &["foo"],
                    &[],
                )
            })
            .collect::<Vec<_>>();
        let cfg = RouterConfig {
            max_skills: 3,
            ..Default::default()
        };
        let r = route(&sks, "foo", &cfg, false);
        assert_eq!(r.selected.len(), 3);
    }
}
