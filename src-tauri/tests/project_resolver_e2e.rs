//! End-to-end resolver scenarios against real on-disk fake projects.

use pigide_lib::project_resolver::indexer::{scan, ScanOptions};
use pigide_lib::project_resolver::resolver::{resolve, ResolveContext, ResolveStatus};
use std::fs;
use std::path::{Path, PathBuf};

fn tmp(suffix: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "pigide-resolver-e2e-{}-{}",
        suffix,
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&p).unwrap();
    p
}

fn make(root: &Path, name: &str, files: &[(&str, &str)]) {
    let p = root.join(name);
    fs::create_dir_all(&p).unwrap();
    for (fname, body) in files {
        let path = p.join(fname);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, body).unwrap();
    }
}

fn corpus(root: &Path) {
    make(
        root,
        "drugs-tracker-plugin",
        &[
            ("Cargo.toml", "[package]\nname = \"drugs_tracker\"\n"),
            (
                "README.md",
                "# Drugs Plugin\n\nPaper plugin to track drugs\n",
            ),
            ("paper-plugin.yml", "name: DrugsPlugin\nversion: 1.0\n"),
        ],
    );
    make(
        root,
        "pigide",
        &[
            ("Cargo.toml", "[package]\nname = \"pigide\"\n"),
            ("README.md", "# PigIDE\n\nDesktop IDE\n"),
        ],
    );
    make(
        root,
        "fancy-dashboard",
        &[
            (
                "package.json",
                r#"{"name":"fancy-dashboard","description":"web dashboard"}"#,
            ),
            ("README.md", "# Fancy Dashboard\n"),
        ],
    );
    make(
        root,
        "pigide-mobile",
        &[("Cargo.toml", "[package]\nname = \"pigide_mobile\"\n")],
    );
    make(
        root,
        "totally-unrelated-thing",
        &[(
            "package.json",
            r#"{"name":"totally-unrelated-thing","description":"unrelated"}"#,
        )],
    );

    // Add an alias for drugs-tracker-plugin.
    let drugs = root.join("drugs-tracker-plugin").join(".pigmemory");
    fs::create_dir_all(&drugs).unwrap();
    fs::write(
        drugs.join("aliases.json"),
        r#"{"aliases":["drugs","drugs plugin","наркотики"]}"#,
    )
    .unwrap();
}

#[test]
fn cold_scan_under_budget() {
    let root = tmp("budget");
    corpus(&root);
    let started = std::time::Instant::now();
    let idx = scan(ScanOptions {
        roots: vec![root.clone()],
        max_depth: 4,
        home_max_depth: 2,
    });
    assert_eq!(idx.projects.len(), 5, "{:?}", idx.projects);
    assert!(
        started.elapsed().as_secs() < 3,
        "cold scan too slow: {:?}",
        started.elapsed()
    );
    fs::remove_dir_all(&root).ok();
}

#[test]
fn fifteen_realistic_queries() {
    let root = tmp("scenarios");
    corpus(&root);
    let idx = scan(ScanOptions {
        roots: vec![root.clone()],
        max_depth: 4,
        home_max_depth: 2,
    });
    let ctx = ResolveContext::default();

    // (query, expected_dirname_or_none)
    let cases: Vec<(&str, Option<&str>)> = vec![
        ("drugs plugin", Some("drugs-tracker-plugin")),
        ("drug plgn", Some("drugs-tracker-plugin")),
        ("drugs", Some("drugs-tracker-plugin")),
        ("DrugsPlugin", Some("drugs-tracker-plugin")),
        ("наркотики", Some("drugs-tracker-plugin")),
        ("наркотики плагин", Some("drugs-tracker-plugin")),
        ("pigide", Some("pigide")),
        ("PigIDE", Some("pigide")),
        ("pgide", Some("pigide")),
        ("fancy", Some("fancy-dashboard")),
        ("dashboard", Some("fancy-dashboard")),
        ("kettlebell-routine-zzz", None), // not_found
        ("xyzzy frobnicator", None),      // not_found
        ("totally", Some("totally-unrelated-thing")),
        // ambiguous between pigide and pigide-mobile is acceptable —
        // either Found(pigide) or Ambiguous is fine, but pigide must
        // be in the top-K.
    ];

    for (q, expect) in cases {
        let r = resolve(q, &idx, &ctx);
        match (expect, &r.status) {
            (Some(name), ResolveStatus::Found) | (Some(name), ResolveStatus::Ambiguous) => {
                let in_top = r.candidates.iter().any(|c| c.dirname == name);
                assert!(in_top, "query={:?} top={:?}", q, r.candidates);
            }
            (None, ResolveStatus::NotFound) => {}
            other => panic!(
                "query={:?} expected={:?} got={:?} candidates={:?}",
                q, expect, other.1, r.candidates
            ),
        }
    }
    fs::remove_dir_all(&root).ok();
}

#[test]
fn warm_resolve_under_50ms() {
    let root = tmp("warm");
    corpus(&root);
    // Inflate the corpus a bit to be closer to a real machine.
    for i in 0..50 {
        make(
            &root,
            &format!("noise-{}", i),
            &[("Cargo.toml", "[package]\nname=\"x\"")],
        );
    }
    let idx = scan(ScanOptions {
        roots: vec![root.clone()],
        max_depth: 4,
        home_max_depth: 2,
    });

    // warm = index already in memory; resolve must be fast even at scale.
    let started = std::time::Instant::now();
    let _r = resolve("drug plgn", &idx, &ResolveContext::default());
    let elapsed = started.elapsed();
    assert!(elapsed.as_millis() < 50, "warm resolve took {:?}", elapsed);
    fs::remove_dir_all(&root).ok();
}

#[test]
fn ambiguity_surfaces_topk() {
    let root = tmp("ambig");
    make(&root, "alpha", &[("Cargo.toml", "[package]\nname=\"a\"")]);
    make(
        &root,
        "alpha-2",
        &[("Cargo.toml", "[package]\nname=\"a2\"")],
    );
    make(
        &root,
        "alpha-3",
        &[("Cargo.toml", "[package]\nname=\"a3\"")],
    );
    let idx = scan(ScanOptions {
        roots: vec![root.clone()],
        max_depth: 4,
        home_max_depth: 2,
    });
    let r = resolve("alpha", &idx, &ResolveContext::default());
    // We don't pin the exact branch (Found vs Ambiguous depends on the
    // dirname-substring tie-break), but the top-K must contain all three.
    assert_eq!(r.candidates.len(), 3);
    let names: Vec<_> = r.candidates.iter().map(|c| c.dirname.clone()).collect();
    assert!(names.iter().any(|n| n == "alpha"));
    assert!(names.iter().any(|n| n == "alpha-2"));
    assert!(names.iter().any(|n| n == "alpha-3"));
    fs::remove_dir_all(&root).ok();
}
