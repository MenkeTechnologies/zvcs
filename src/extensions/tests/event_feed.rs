//! The event feed (`git zevents`, its alias `git ztail`, and `git zsince`) and
//! the filter grammar all three share: `-n <count>`, `--kind`, `--repo`,
//! `--json`, `--no-follow`.
//!
//! The feed is the only record of what the tree did while nobody was watching,
//! so a filter that quietly matches the wrong set is worse than one that errors.
//! Two shapes are pinned here:
//!
//!   * every advertised `--kind` has a real writer — `stage` from `git add`,
//!     `commit` and `status` from the `repo_status` triggers — so a documented
//!     kind cannot become one that never matches;
//!   * `--repo` is a literal case-insensitive substring. The queries filter with
//!     SQL `LIKE`, where `_` and `%` are metacharacters: unescaped, `--repo
//!     my_repo` also matches `myXrepo` and `--repo %` matches the whole tree,
//!     while `git zon --repo`, which runs a command per match, used a
//!     case-sensitive Rust `contains` and so fired on a different set than the
//!     feed previewed.
//!
//! `reconcile` events are written by the daemon's autonomy path, so they are out
//! of reach here; the kinds below are the three a plain command can produce.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(home: &Path, dir: &Path, args: &[&str]) {
    // ZVCS_HOME on every call: `git add` writes the `stage` event itself, so a
    // plain command without it would record into the real index instead.
    let ok = Command::new(BIN).args(args).current_dir(dir).env("ZVCS_HOME", home).status().unwrap().success();
    assert!(ok, "git {args:?} failed");
}

fn feed(home: &Path, cwd: &Path, args: &[&str]) -> String {
    let out = Command::new(BIN).args(args).current_dir(cwd).env("ZVCS_HOME", home).output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Every `"key":value` for `key` across NDJSON lines, in order.
fn field(ndjson: &str, key: &str) -> Vec<String> {
    ndjson
        .lines()
        .filter_map(|l| {
            let at = l.find(&format!("\"{key}\":"))? + key.len() + 3;
            let tail = &l[at..];
            let end = tail.find(',').unwrap_or(tail.len() - 1);
            Some(tail[..end].trim_matches('"').to_string())
        })
        .collect()
}

/// Repos whose names differ only by a character where `my_repo` vs `myXrepo`
/// separates a literal `_` from a LIKE wildcard.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-feed-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    for name in ["my_repo", "myXrepo"] {
        let r = root.join(name);
        std::fs::create_dir_all(&r).unwrap();
        git(&home, &r, &["init", "-q", "-b", "main"]);
        std::fs::write(r.join("f.txt"), b"1\n").unwrap();
        git(&home, &r, &["add", "f.txt"]); // → a `stage` event per repo
        git(&home, &r, &["commit", "-q", "-m", "c0"]);
    }
    feed(&home, &root, &["zreindex", "--sync", root.to_str().unwrap()]);
    (root, home)
}

#[test]
fn every_advertised_kind_has_a_writer() {
    let (root, home) = fixture("kinds");
    let one = root.join("my_repo");

    // The two trigger-written kinds need distinct transitions on `repo_status`:
    // `commit` fires when head_sha moves, `status` only when the dirty flag or
    // the sync state actually changes, so the repo must be observed dirty and
    // then observed clean again.
    std::fs::write(one.join("f.txt"), b"2\n").unwrap();
    feed(&home, &one, &["zstatus"]); // dirty
    git(&home, &one, &["commit", "-qam", "c1"]); // moves head_sha
    feed(&home, &one, &["zstatus"]); // clean again → dirty transition + commit

    let all = feed(&home, &root, &["zevents", "--no-follow", "--json", "-n", "100"]);
    let kinds = field(&all, "kind");
    for want in ["stage", "commit", "status"] {
        assert!(kinds.iter().any(|k| k == want), "no `{want}` event was ever written:\n{all}");
        // The filter must isolate exactly that kind — never empty, never mixed.
        let only = feed(&home, &root, &["zevents", "--no-follow", "--json", "--kind", want, "-n", "100"]);
        let got = field(&only, "kind");
        assert!(!got.is_empty(), "--kind {want} matched nothing though the feed has one:\n{all}");
        assert!(got.iter().all(|k| k == want), "--kind {want} let another kind through:\n{only}");
    }

    // A kind nobody writes selects nothing rather than falling back to all.
    let bogus = feed(&home, &root, &["zevents", "--no-follow", "--json", "--kind", "nosuchkind"]);
    assert!(bogus.trim().is_empty(), "an unknown --kind must match nothing:\n{bogus}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn repo_filter_is_a_literal_substring() {
    let (root, home) = fixture("repo");

    // `my_repo` names one repo literally; `_` must not match the `X` in the
    // sibling, or the filter silently doubles the set it reports.
    let underscore = feed(&home, &root, &["zevents", "--no-follow", "--json", "--repo", "my_repo", "-n", "100"]);
    let repos = field(&underscore, "repo");
    assert!(!repos.is_empty(), "--repo my_repo must match its own repo:\n{underscore}");
    assert!(repos.iter().all(|r| r.ends_with("my_repo")),
        "`_` acted as a LIKE wildcard and matched myXrepo:\n{underscore}");

    // A bare `%` is a path substring no repo contains, not "everything".
    let pct = feed(&home, &root, &["zevents", "--no-follow", "--json", "--repo", "%", "-n", "100"]);
    assert!(pct.trim().is_empty(), "`%` acted as a LIKE wildcard and matched the tree:\n{pct}");

    // Case-insensitive, like the fleet selectors.
    let upper = feed(&home, &root, &["zevents", "--no-follow", "--json", "--repo", "MYXREPO", "-n", "100"]);
    assert!(field(&upper, "repo").iter().all(|r| r.ends_with("myXrepo")) && !upper.trim().is_empty(),
        "--repo must fold case:\n{upper}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn zsince_shares_the_filter_grammar() {
    let (root, home) = fixture("since");

    // Same two probes through the other query (`events_after_ts`), which builds
    // its own SQL and so can drift from the tail's.
    let win = "1h";
    let underscore = feed(&home, &root, &["zsince", win, "--json", "--repo", "my_repo"]);
    assert!(!underscore.trim().is_empty(), "zsince must see the backlog:\n{underscore}");
    assert!(field(&underscore, "repo").iter().all(|r| r.ends_with("my_repo")),
        "zsince --repo let `_` act as a wildcard:\n{underscore}");
    let pct = feed(&home, &root, &["zsince", win, "--json", "--repo", "%"]);
    assert!(pct.trim().is_empty(), "zsince --repo `%` matched the whole tree:\n{pct}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn backlog_is_bounded_newest_and_ordered_oldest_first() {
    let (root, home) = fixture("bound");
    let one = root.join("my_repo");

    // Five more stage events, so the backlog has more than the window asks for.
    for i in 0..5 {
        std::fs::write(one.join("f.txt"), format!("{i}\n")).unwrap();
        git(&home, &one, &["add", "f.txt"]);
    }

    let all: Vec<i64> = field(&feed(&home, &root, &["zevents", "--no-follow", "--json", "-n", "100"]), "id")
        .iter()
        .map(|s| s.parse().unwrap())
        .collect();
    assert!(all.len() > 3, "fixture must produce more events than the window:\n{all:?}");
    assert!(all.windows(2).all(|w| w[0] < w[1]), "backlog must read oldest-first: {all:?}");

    let window: Vec<i64> = field(&feed(&home, &root, &["zevents", "--no-follow", "--json", "-n", "3"]), "id")
        .iter()
        .map(|s| s.parse().unwrap())
        .collect();
    assert_eq!(window.len(), 3, "-n 3 must bound the backlog to three: {window:?}");
    assert_eq!(window, all[all.len() - 3..], "-n 3 must keep the NEWEST three, still oldest-first");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn ztail_is_the_same_verb_and_json_carries_the_documented_fields() {
    let (root, home) = fixture("alias");

    let args = ["--no-follow", "--json", "-n", "5"];
    let as_events = feed(&home, &root, &[&["zevents"], &args[..]].concat());
    let as_tail = feed(&home, &root, &[&["ztail"], &args[..]].concat());
    assert!(!as_events.trim().is_empty(), "precondition: the feed has a backlog");
    assert_eq!(as_events, as_tail, "`ztail` is documented as an alias of `zevents` and must not drift");

    // `-1` is the documented short spelling of --no-follow: it must return
    // rather than hang, and print the same backlog.
    let short = feed(&home, &root, &["ztail", "-1", "--json", "-n", "5"]);
    assert_eq!(short, as_tail, "`-1` must mean --no-follow");

    let line = as_events.lines().next().unwrap();
    for key in ["id", "ts", "kind", "repo", "detail", "before", "after"] {
        assert!(line.contains(&format!("\"{key}\":")), "NDJSON is missing `{key}`:\n{line}");
    }

    let _ = std::fs::remove_dir_all(&root);
}
