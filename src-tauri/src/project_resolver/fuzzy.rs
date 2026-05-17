//! Lightweight fuzzy scoring helpers (no external deps).
//!
//! We deliberately avoid `strsim` / `rapidfuzz` to keep the dependency
//! footprint small. The implementations below are good enough for the
//! ~200-project corpus the resolver works with: each call is O(n*m)
//! where n,m ≤ 64.

/// Classic Jaro similarity in [0,1].
pub fn jaro(a: &str, b: &str) -> f64 {
    let av: Vec<char> = a.chars().collect();
    let bv: Vec<char> = b.chars().collect();
    let (alen, blen) = (av.len(), bv.len());
    if alen == 0 && blen == 0 {
        return 1.0;
    }
    if alen == 0 || blen == 0 {
        return 0.0;
    }
    let match_window = (alen.max(blen) / 2).saturating_sub(1);
    let mut a_matches = vec![false; alen];
    let mut b_matches = vec![false; blen];
    let mut matches = 0usize;

    for i in 0..alen {
        let lo = i.saturating_sub(match_window);
        let hi = (i + match_window + 1).min(blen);
        for j in lo..hi {
            if b_matches[j] {
                continue;
            }
            if av[i] != bv[j] {
                continue;
            }
            a_matches[i] = true;
            b_matches[j] = true;
            matches += 1;
            break;
        }
    }
    if matches == 0 {
        return 0.0;
    }

    let mut transpositions = 0usize;
    let mut k = 0usize;
    for i in 0..alen {
        if !a_matches[i] {
            continue;
        }
        while !b_matches[k] {
            k += 1;
        }
        if av[i] != bv[k] {
            transpositions += 1;
        }
        k += 1;
    }
    let m = matches as f64;
    let t = (transpositions as f64) / 2.0;
    ((m / alen as f64) + (m / blen as f64) + ((m - t) / m)) / 3.0
}

/// Jaro-Winkler with prefix scaling factor 0.1 and a 4-char prefix cap —
/// the standard parameter set.
pub fn jaro_winkler(a: &str, b: &str) -> f64 {
    let j = jaro(a, b);
    if j == 0.0 {
        return 0.0;
    }
    let mut prefix = 0usize;
    for (ca, cb) in a.chars().zip(b.chars()) {
        if ca == cb {
            prefix += 1;
            if prefix == 4 {
                break;
            }
        } else {
            break;
        }
    }
    j + (prefix as f64) * 0.1 * (1.0 - j)
}

/// Split on any non-alphanumeric character. Empty / single-char tokens
/// are dropped — single chars (`"x"` placeholders) carry no signal and
/// inflate token-set scores unfairly.
pub fn tokens(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= 2)
        .map(|t| t.to_string())
        .collect()
}

/// rapidfuzz-style token-set ratio.
///
/// Splits both sides into token sets, finds the intersection, and runs
/// Jaro-Winkler over (intersection vs intersection∪diff_a) and
/// (intersection vs intersection∪diff_b). Returns the higher score so a
/// missing token in either query or signal is forgiven.
///
/// As a fallback for token corpora with no exact-match overlap (e.g. "drug"
/// vs "drugs"), we also run a pairwise jaro_winkler max so near-misses
/// still produce a reasonable score.
pub fn token_set_ratio(a: &str, b: &str) -> f64 {
    let ta_v = tokens(a);
    let tb_v = tokens(b);
    let ta: std::collections::BTreeSet<String> = ta_v.iter().cloned().collect();
    let tb: std::collections::BTreeSet<String> = tb_v.iter().cloned().collect();
    if ta.is_empty() && tb.is_empty() {
        return 1.0;
    }
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    let inter: Vec<&String> = ta.intersection(&tb).collect();
    let diff_a: Vec<&String> = ta.difference(&tb).collect();
    let diff_b: Vec<&String> = tb.difference(&ta).collect();

    let join = |xs: &[&String]| xs.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" ");
    let s_inter = join(&inter);
    let s_a = if diff_a.is_empty() {
        s_inter.clone()
    } else if s_inter.is_empty() {
        join(&diff_a)
    } else {
        format!("{} {}", s_inter, join(&diff_a))
    };
    let s_b = if diff_b.is_empty() {
        s_inter.clone()
    } else if s_inter.is_empty() {
        join(&diff_b)
    } else {
        format!("{} {}", s_inter, join(&diff_b))
    };

    let r1 = jaro_winkler(&s_inter, &s_a);
    let r2 = jaro_winkler(&s_inter, &s_b);
    let r3 = jaro_winkler(&s_a, &s_b);
    let mut best = r1.max(r2).max(r3);

    // Pairwise "best buddy" — each query token contributes its highest
    // match against any signal token. Pair scores below 0.75 are
    // discarded so unrelated tokens don't pull the mean up via the
    // Jaro-Winkler floor.
    let mut sum = 0.0;
    let mut n = 0usize;
    for ta_t in &ta_v {
        let mut local: f64 = 0.0;
        for tb_t in &tb_v {
            local = local.max(jaro_winkler(ta_t, tb_t));
        }
        if local < 0.75 {
            local = 0.0;
        }
        sum += local;
        n += 1;
    }
    if n > 0 {
        let buddy = sum / (n as f64);
        if buddy > best {
            best = buddy;
        }
    }
    best
}

/// True if every token of `a` appears (as a substring) in `b`.
pub fn all_tokens_contained(a: &str, b: &str) -> bool {
    let ta = tokens(a);
    if ta.is_empty() {
        return false;
    }
    ta.iter().all(|t| b.contains(t))
}

/// Combined score used by the resolver. Returns the max of:
/// * Jaro-Winkler on the raw normalized strings, scaled by a
///   length-balance factor so a 1-char signal can't ride a 9-char query
///   to a near-perfect score,
/// * token-set ratio (already balanced by construction),
/// * a prefix bonus: if the shorter side is a strict prefix of the
///   longer side and is at least 3 chars long, score floors at 0.9.
pub fn fuzzy_score(query: &str, signal: &str) -> f64 {
    if query.is_empty() || signal.is_empty() {
        return 0.0;
    }
    let q_len = query.chars().count();
    let s_len = signal.chars().count();
    let balance = q_len.min(s_len) as f64 / q_len.max(s_len) as f64;

    let mut prefix_floor = 0.0_f64;
    if q_len.min(s_len) >= 3 && (signal.starts_with(query) || query.starts_with(signal)) {
        prefix_floor = 0.88;
    }

    let scaled_jw = jaro_winkler(query, signal) * (0.5 + 0.5 * balance);
    let ts = token_set_ratio(query, signal);
    prefix_floor.max(ts.max(scaled_jw))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jaro_basics() {
        assert!((jaro("MARTHA", "MARHTA") - 0.9444).abs() < 0.01);
        assert_eq!(jaro("", ""), 1.0);
        assert_eq!(jaro("", "abc"), 0.0);
    }

    #[test]
    fn jaro_winkler_prefix_boost() {
        let a = jaro("DWAYNE", "DUANE");
        let b = jaro_winkler("DWAYNE", "DUANE");
        assert!(b >= a);
    }

    #[test]
    fn token_set_handles_reordering() {
        let s = token_set_ratio("plugin drugs", "drugs plugin");
        assert!(s >= 0.95, "score={}", s);
    }

    #[test]
    fn fuzzy_typo_match() {
        // "drug plgn" → "drugs-tracker-plugin"
        let s = fuzzy_score("drug plgn", "drugs-tracker-plugin");
        assert!(s >= 0.7, "score={}", s);
    }

    #[test]
    fn fuzzy_prefix() {
        let s = fuzzy_score("pig", "pigide");
        assert!(s >= 0.85, "score={}", s);
    }

    #[test]
    fn fuzzy_unrelated_low() {
        let s = fuzzy_score("kettlebell", "drugs-tracker-plugin");
        assert!(s < 0.6, "score={}", s);
    }

    #[test]
    fn all_tokens_contained_basic() {
        assert!(all_tokens_contained("drugs plugin", "drugs-tracker-plugin"));
        assert!(!all_tokens_contained("kettle plugin", "drugs-tracker-plugin"));
    }
}
