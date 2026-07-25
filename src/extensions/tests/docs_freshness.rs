//! The published HTML docs must not drift from the tree they describe.
//!
//! Every number in `docs/` is a claim about this repository — the version, how
//! many verbs are dispatched, which `[zvcs]` switches exist. Those change often
//! and the pages do not change with them: the docs hub and the engineering
//! report both sat at "v0.1.0" and "3 superset verbs" long after the tree had
//! grown past a hundred, which is the sort of thing a reader has no way to
//! detect and no reason to doubt.
//!
//! These tests read the source of truth (the dispatch tables, the crate
//! version, the config reader) and fail when a page disagrees with it. They
//! deliberately assert on the FACTS, never on prose — a page is free to be
//! rewritten as long as what it claims is still true.

use std::path::PathBuf;
use zvcs::dispatch::{PORCELAIN_VERBS, SUPERSET_VERBS};

/// Pages that carry factual claims about the tree.
const PAGES: [&str; 2] = ["docs/index.html", "docs/report.html"];

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is `<root>/src/extensions`.
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn page(name: &str) -> String {
    let path = repo_root().join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// A page that prints a version must print THIS version. Both pages carry it in
/// a build line, and the report also renders it as a headline stat.
#[test]
fn published_version_matches_the_crate() {
    let current = format!("v{}", env!("CARGO_PKG_VERSION"));
    let stale = regex::Regex::new(r"\bv0\.\d+\.\d+\b").expect("version pattern");

    for name in PAGES {
        let html = page(name);
        let found: Vec<&str> = stale.find_iter(&html).map(|m| m.as_str()).collect();
        if found.is_empty() {
            continue; // a page is allowed to name no version at all
        }
        for v in &found {
            assert_eq!(
                *v, current,
                "{name} advertises {v}, but the crate is {current} — regenerate or edit the page"
            );
        }
    }
}

/// The verb counts the docs quote must be the counts the dispatcher actually
/// has. Both pages state them, in a stat card and in the dispatch diagram.
#[test]
fn quoted_verb_counts_match_the_dispatch_tables() {
    let superset = SUPERSET_VERBS.len();
    let porcelain = PORCELAIN_VERBS.len();

    // Compile the scan patterns once, not once per page (clippy::regex_creation_in_loops).
    let z_re = regex::Regex::new(r"z\*, (\d+)\)").expect("z* pattern");
    let porcelain_re = regex::Regex::new(r"porcelain \((\d+)\)").expect("porcelain pattern");
    let stat_re = regex::Regex::new(r#"stat-val">(\d+)</div><div class="stat-label">([^<]+)<"#)
        .expect("stat pattern");

    for name in PAGES {
        let html = page(name);
        // "superset verbs (z*, N)" in the dispatch diagrams.
        for cap in z_re.captures_iter(&html) {
            let claimed: usize = cap[1].parse().expect("count is a number");
            assert_eq!(claimed, superset, "{name} says {claimed} superset verbs, there are {superset}");
        }
        // "porcelain (N)" in the same diagrams.
        for cap in porcelain_re.captures_iter(&html) {
            let claimed: usize = cap[1].parse().expect("count is a number");
            assert_eq!(claimed, porcelain, "{name} says {claimed} git-compat verbs, there are {porcelain}");
        }
        // The report's headline stat cards.
        for cap in stat_re.captures_iter(&html) {
            let claimed: usize = cap[1].parse().expect("count is a number");
            let label = &cap[2];
            let expected = match label {
                l if l.contains("superset") => superset,
                l if l.contains("git-compat") => porcelain,
                _ => continue,
            };
            assert_eq!(claimed, expected, "{name}: stat card '{label}' says {claimed}, actual {expected}");
        }
    }
}

/// Every `[zvcs]` switch the docs name must be read somewhere in the tree, and
/// every switch the tree reads must be documented. A key that quietly stops
/// being read is worse than an undocumented one: the page keeps telling people
/// to set something inert.
#[test]
fn documented_zvcs_switches_match_the_ones_the_tree_reads() {
    // Scan the whole crate, not just `config.rs` — some switches are read by the
    // module that owns them (the status maintainer, the watcher, the REPL) rather
    // than by the shared config struct. Only READS count: the key has to appear
    // inside a config lookup, so that `zvcs.log` and `zvcs.sock` (file names under
    // ~/.zvcs, not settings) are not mistaken for switches.
    let key = regex::Regex::new(
        r#"(?s)(?:boolean|string|integer|config_bool|config_str|config_int)\(\s*"zvcs\.([a-z]+)""#,
    )
    .expect("config key pattern");
    let mut real: Vec<String> = Vec::new();
    let src = repo_root().join("src").join("extensions").join("src");
    let mut dirs = vec![src];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).expect("read source dir").flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).expect("read source file");
                real.extend(key.captures_iter(&text).map(|c| c[1].to_string()));
            }
        }
    }
    real.sort();
    real.dedup();
    assert!(!real.is_empty(), "no zvcs.* keys found — did the config reader move?");

    // The hub documents the switch set in one paragraph; find the keys it names.
    let html = page("docs/index.html");
    let block = html
        .split("<code>[zvcs]</code> gitconfig")
        .nth(1)
        .expect("docs/index.html no longer documents the [zvcs] switches")
        .split("</p>")
        .next()
        .expect("unterminated paragraph")
        // The paragraph ends with prose that mentions `git` itself; the switch
        // listing is everything before it.
        .split("Headless failures")
        .next()
        .expect("switch listing");
    let named: Vec<String> = regex::Regex::new(r"<code>([a-z]+)</code>")
        .expect("code pattern")
        .captures_iter(block)
        .map(|c| c[1].to_string())
        .collect();

    for k in &real {
        // `interval` is read directly rather than through a documented switch
        // name in some paths; every other key must be named on the page.
        assert!(
            named.iter().any(|n| n == k),
            "docs/index.html does not document the `zvcs.{k}` switch (documented: {named:?})"
        );
    }
    for n in &named {
        assert!(
            real.iter().any(|k| k == n),
            "docs/index.html documents `zvcs.{n}`, which nothing in the tree reads"
        );
    }
}
