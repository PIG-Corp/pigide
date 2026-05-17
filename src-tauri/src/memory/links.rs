//! `[[wikilinks]]` extraction and resolution.

use once_cell::sync::Lazy;
use regex::Regex;

/// `[[target]]` or `[[target|display text]]`. Targets may not contain `]`,
/// `[`, `|`, or newlines.
static WIKILINK: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\[\[([^\[\]\|\n]+?)(?:\|([^\[\]\n]+?))?\]\]").expect("wikilink regex")
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WikiRef {
    pub target: String,
    pub display: Option<String>,
}

/// Extract every wikilink from a body. Order preserved, duplicates kept —
/// callers may dedupe by target if needed.
pub fn extract(body: &str) -> Vec<WikiRef> {
    WIKILINK
        .captures_iter(body)
        .map(|c| WikiRef {
            target: c.get(1).map(|m| m.as_str().trim().to_string()).unwrap_or_default(),
            display: c.get(2).map(|m| m.as_str().trim().to_string()),
        })
        .collect()
}

/// A candidate note used for resolution.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub id: String,
    pub slug: String,
    pub title: String,
    pub aliases: Vec<String>,
}

/// Resolution outcome for a single wikilink.
#[derive(Debug, Clone)]
pub enum Resolution {
    Resolved { id: String },
    Ambiguous { ids: Vec<String> },
    Unresolved,
}

/// Resolve a target string against a slice of candidates. Order:
/// 1. Exact slug match.
/// 2. Alias match (case-insensitive).
/// 3. Title match (case-insensitive).
pub fn resolve(target: &str, candidates: &[Candidate]) -> Resolution {
    let t = target.trim();
    let lower = t.to_lowercase();

    // 1. Slug exact.
    let by_slug: Vec<_> = candidates.iter().filter(|c| c.slug == t).collect();
    if by_slug.len() == 1 {
        return Resolution::Resolved {
            id: by_slug[0].id.clone(),
        };
    }
    if by_slug.len() > 1 {
        return Resolution::Ambiguous {
            ids: by_slug.into_iter().map(|c| c.id.clone()).collect(),
        };
    }

    // 2. Alias case-insensitive.
    let by_alias: Vec<_> = candidates
        .iter()
        .filter(|c| c.aliases.iter().any(|a| a.eq_ignore_ascii_case(t)))
        .collect();
    if by_alias.len() == 1 {
        return Resolution::Resolved {
            id: by_alias[0].id.clone(),
        };
    }
    if by_alias.len() > 1 {
        return Resolution::Ambiguous {
            ids: by_alias.into_iter().map(|c| c.id.clone()).collect(),
        };
    }

    // 3. Title case-insensitive.
    let by_title: Vec<_> = candidates
        .iter()
        .filter(|c| c.title.to_lowercase() == lower)
        .collect();
    if by_title.len() == 1 {
        return Resolution::Resolved {
            id: by_title[0].id.clone(),
        };
    }
    if by_title.len() > 1 {
        return Resolution::Ambiguous {
            ids: by_title.into_iter().map(|c| c.id.clone()).collect(),
        };
    }

    Resolution::Unresolved
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_simple_and_aliased() {
        let body = "see [[auth-pattern]] and also [[stripe|Stripe webhook]] later";
        let v = extract(body);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].target, "auth-pattern");
        assert!(v[0].display.is_none());
        assert_eq!(v[1].target, "stripe");
        assert_eq!(v[1].display.as_deref(), Some("Stripe webhook"));
    }

    #[test]
    fn ignores_malformed() {
        let body = "[[ok]] [[broken [[ok2]]";
        let v = extract(body);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].target, "ok");
        assert_eq!(v[1].target, "ok2");
    }

    #[test]
    fn resolve_by_slug_alias_title() {
        let cs = vec![
            Candidate {
                id: "1".into(),
                slug: "auth-pattern".into(),
                title: "Auth pattern".into(),
                aliases: vec!["authn".into()],
            },
            Candidate {
                id: "2".into(),
                slug: "other".into(),
                title: "Other".into(),
                aliases: vec![],
            },
        ];
        match resolve("auth-pattern", &cs) {
            Resolution::Resolved { id } => assert_eq!(id, "1"),
            _ => panic!("expected resolved"),
        }
        match resolve("AUTHN", &cs) {
            Resolution::Resolved { id } => assert_eq!(id, "1"),
            _ => panic!("expected alias"),
        }
        match resolve("auth pattern", &cs) {
            Resolution::Resolved { id } => assert_eq!(id, "1"),
            _ => panic!("expected title"),
        }
        match resolve("ghost", &cs) {
            Resolution::Unresolved => {}
            _ => panic!("expected unresolved"),
        }
    }
}
