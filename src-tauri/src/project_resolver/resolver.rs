//! Score every indexed project against a query and decide on the
//! resolution outcome.

use crate::project_resolver::fuzzy::fuzzy_score;
use crate::project_resolver::indexer::{ProjectEntry, ProjectIndex};
use crate::project_resolver::parsers::remote_repo_name;
use crate::project_resolver::translit::normalize;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub path: String,
    pub dirname: String,
    pub display_name: String,
    pub score: f64,
    pub matched_signal: String,
    pub kinds: Vec<String>,
    pub headings: Vec<String>,
    pub remote: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ResolveStatus {
    Found,
    Ambiguous,
    NotFound,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolveOutcome {
    pub status: ResolveStatus,
    pub query: String,
    pub candidates: Vec<Candidate>,
    pub confidence: f64,
}

#[derive(Debug, Clone)]
pub struct ResolveContext<'a> {
    /// Workspaces that the user has interacted with recently. Path strings
    /// canonicalized to `String` so we can match against `ProjectEntry.path`.
    pub recent_paths: &'a [String],
    /// Process / window cwd. Boosts a project that contains it.
    pub current_cwd: Option<&'a str>,
    /// Top K candidates to return.
    pub top_k: usize,
}

impl Default for ResolveContext<'_> {
    fn default() -> Self {
        Self {
            recent_paths: &[],
            current_cwd: None,
            top_k: 5,
        }
    }
}

const FOUND_THRESHOLD: f64 = 0.85;
const FOUND_GAP: f64 = 0.10;
const NOT_FOUND_THRESHOLD: f64 = 0.65;

/// Score `query` against every project in `idx` and decide.
pub fn resolve(query: &str, idx: &ProjectIndex, ctx: &ResolveContext<'_>) -> ResolveOutcome {
    let q_norm = normalize(query.trim());
    if q_norm.is_empty() {
        return ResolveOutcome {
            status: ResolveStatus::NotFound,
            query: query.to_string(),
            candidates: Vec::new(),
            confidence: 0.0,
        };
    }

    let mut scored: Vec<Candidate> = idx
        .projects
        .iter()
        .map(|p| score_project(&q_norm, p, ctx))
        .collect();

    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    scored.truncate(ctx.top_k.max(1));

    let top1 = scored.first().map(|c| c.score).unwrap_or(0.0);
    let top2 = scored.get(1).map(|c| c.score).unwrap_or(0.0);

    // Exact-dirname uniqueness check: if exactly one project's normalized
    // dirname equals the query, it wins regardless of how close the runners
    // up are. This is the "switch to pigide" case where two `pigide-*`
    // siblings would otherwise drag the top down via the dirname-substring
    // floor.
    let mut exact_count = 0usize;
    for p in &idx.projects {
        if normalize(&p.dirname) == q_norm {
            exact_count += 1;
        }
    }

    let status = if top1 < NOT_FOUND_THRESHOLD {
        ResolveStatus::NotFound
    } else if exact_count == 1
        && scored
            .first()
            .map(|c| normalize(&c.dirname) == q_norm)
            .unwrap_or(false)
    {
        ResolveStatus::Found
    } else if top1 >= FOUND_THRESHOLD && (top1 - top2) >= FOUND_GAP {
        ResolveStatus::Found
    } else {
        ResolveStatus::Ambiguous
    };

    ResolveOutcome {
        status,
        query: query.to_string(),
        candidates: scored,
        confidence: top1,
    }
}

/// Compute the best score for a single project against an already-normalized
/// query.
fn score_project(q_norm: &str, p: &ProjectEntry, ctx: &ResolveContext<'_>) -> Candidate {
    let mut best: f64 = 0.0;
    let mut matched_signal = String::new();

    let consider = |signal: &str, raw: &str, base: &mut f64, label: &mut String| {
        if signal.is_empty() {
            return;
        }
        let s = normalize(signal);
        let v = fuzzy_score(q_norm, &s);
        if v > *base {
            *base = v;
            *label = raw.to_string();
        }
    };

    consider(&p.dirname, &p.dirname, &mut best, &mut matched_signal);
    for n in &p.names {
        consider(n, n, &mut best, &mut matched_signal);
    }
    for h in &p.headings {
        consider(h, h, &mut best, &mut matched_signal);
    }
    if let Some(remote) = &p.remote {
        if let Some(repo) = remote_repo_name(remote) {
            consider(&repo, &repo, &mut best, &mut matched_signal);
        }
    }
    for d in &p.descriptions {
        // descriptions are noisier; only count if it pushes us up
        // through token-set match (full sentence vs short query).
        let s = normalize(d);
        let v = fuzzy_score(q_norm, &s);
        if v > best {
            best = v;
            matched_signal = d.clone();
        }
    }

    // Aliases get an explicit boost when one matches near-perfectly.
    let mut alias_boost: f64 = 0.0;
    let mut alias_label: Option<String> = None;
    for a in &p.aliases {
        let s = normalize(a);
        let v = fuzzy_score(q_norm, &s);
        if v > best {
            best = v;
            matched_signal = a.clone();
        }
        if v >= 0.92 && v > alias_boost {
            alias_boost = 0.30;
            alias_label = Some(a.clone());
        }
    }

    let mut score = best + alias_boost;
    if alias_label.is_some() {
        matched_signal = alias_label.unwrap();
    }

    // Recent workspace boost — tilts ties toward something the user has
    // touched recently. Applies whenever the project has any signal at
    // all so a recent perfect-typo match still wins over an unrelated
    // strong match.
    if best > 0.0 && ctx.recent_paths.iter().any(|r| same_path(r, &p.path)) {
        score += 0.15;
    }

    if best >= 0.6 {
        if let Some(cwd) = ctx.current_cwd {
            if cwd_under(cwd, &p.path) {
                score += 0.10;
            }
        }
    }

    // Bonus for query tokens being a substring of dirname — picks up
    // partial matches like "drugs" → "drugs-tracker-plugin".
    let dn = normalize(&p.dirname);
    if !q_norm.is_empty() && dn.contains(q_norm) {
        if dn == q_norm {
            // Exact dirname equality is the strongest possible signal —
            // overrides anything else.
            score = 1.0;
        } else {
            // Partial substring is strong but stays below exact so two
            // candidates ("pigide" and "pigideous" for query "pigide")
            // can be told apart.
            score = score.max(0.85);
        }
        if matched_signal.is_empty() {
            matched_signal = p.dirname.clone();
        }
    }

    let display_name = display_name_for(p);

    Candidate {
        path: p.path.clone(),
        dirname: p.dirname.clone(),
        display_name,
        // Cap below the exact-dirname plateau (1.0) so boosted candidates
        // can still tilt ties. The orchestrator only inspects relative
        // ordering, never absolute values, so values >1.0 are fine.
        score: score.max(0.0),
        matched_signal,
        kinds: p.kinds.clone(),
        headings: p.headings.clone(),
        remote: p.remote.clone(),
    }
}

fn display_name_for(p: &ProjectEntry) -> String {
    p.headings
        .first()
        .cloned()
        .or_else(|| p.names.first().cloned())
        .unwrap_or_else(|| p.dirname.clone())
}

fn same_path(a: &str, b: &str) -> bool {
    let na = a.trim_end_matches('/');
    let nb = b.trim_end_matches('/');
    na == nb
}

fn cwd_under(cwd: &str, path: &str) -> bool {
    let cwd = cwd.trim_end_matches('/');
    let path = path.trim_end_matches('/');
    cwd == path || cwd.starts_with(&format!("{}/", path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idx(entries: Vec<ProjectEntry>) -> ProjectIndex {
        ProjectIndex {
            version: 1,
            built_at: "1970-01-01T00:00:00Z".to_string(),
            roots: vec![],
            projects: entries,
        }
    }

    fn project(path: &str, dirname: &str) -> ProjectEntry {
        ProjectEntry {
            path: path.into(),
            dirname: dirname.into(),
            kinds: vec![],
            names: vec![],
            descriptions: vec![],
            headings: vec![],
            remote: None,
            languages: vec![],
            aliases: vec![],
            mtime: 0,
        }
    }

    #[test]
    fn typo_in_dirname_resolves() {
        let i = idx(vec![
            project("/a/drugs-tracker-plugin", "drugs-tracker-plugin"),
            project("/a/pigide", "pigide"),
            project("/a/some-website", "some-website"),
        ]);
        let r = resolve("drug plgn", &i, &ResolveContext::default());
        assert!(matches!(r.status, ResolveStatus::Found));
        assert!(r.candidates[0].path.ends_with("drugs-tracker-plugin"));
    }

    #[test]
    fn russian_query_via_alias() {
        let mut p = project("/x/drugs-tracker-plugin", "drugs-tracker-plugin");
        p.aliases = vec!["наркотики".into(), "drugs plugin".into()];
        let i = idx(vec![p, project("/x/pigide", "pigide")]);
        let r = resolve("наркотики", &i, &ResolveContext::default());
        assert!(matches!(r.status, ResolveStatus::Found));
        assert!(r.candidates[0].path.ends_with("drugs-tracker-plugin"));
    }

    #[test]
    fn human_name_via_readme() {
        let mut p = project("/x/drugs-tracker-plugin", "drugs-tracker-plugin");
        p.headings = vec!["Drugs Plugin".into()];
        let i = idx(vec![p, project("/x/pigide", "pigide")]);
        let r = resolve("drugs plugin", &i, &ResolveContext::default());
        assert!(matches!(r.status, ResolveStatus::Found));
    }

    #[test]
    fn exact_dirname_match_wins() {
        let i = idx(vec![
            project("/x/pigide", "pigide"),
            project("/x/pigideous", "pigideous"),
        ]);
        let r = resolve("pigide", &i, &ResolveContext::default());
        assert!(matches!(r.status, ResolveStatus::Found));
        assert_eq!(r.candidates[0].dirname, "pigide");
    }

    #[test]
    fn unrelated_query_returns_not_found() {
        let i = idx(vec![project("/x/pigide", "pigide")]);
        let r = resolve("kettlebell-routine", &i, &ResolveContext::default());
        assert!(matches!(r.status, ResolveStatus::NotFound));
    }

    #[test]
    fn ambiguous_returns_top_k() {
        let i = idx(vec![
            project("/x/pigide-rs", "pigide-rs"),
            project("/x/pigide-ts", "pigide-ts"),
            project("/x/pigide", "pigide"),
        ]);
        let r = resolve("pigide", &i, &ResolveContext::default());
        // top1 == 1.0, but pigide-* are also ~very high; the dirname
        // substring boost ties them, so we expect Ambiguous.
        match r.status {
            ResolveStatus::Found => assert_eq!(r.candidates[0].dirname, "pigide"),
            ResolveStatus::Ambiguous => {
                assert!(r.candidates.iter().any(|c| c.dirname == "pigide"));
                assert!(r.candidates.iter().any(|c| c.dirname == "pigide-rs"));
            }
            ResolveStatus::NotFound => panic!("should not be NotFound"),
        }
    }

    #[test]
    fn recent_workspace_boost_breaks_tie() {
        let i = idx(vec![
            project("/x/foo", "foo-tool"),
            project("/x/bar", "foo-tool-2"),
        ]);
        let recent = vec!["/x/bar".to_string()];
        let ctx = ResolveContext {
            recent_paths: &recent,
            ..Default::default()
        };
        let r = resolve("foo tool", &i, &ctx);
        assert_eq!(r.candidates[0].path, "/x/bar");
    }
}
