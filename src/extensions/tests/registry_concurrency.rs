//! Concurrent writers must not lose each other's registry entries.
//!
//! `zguard`, `zintercept` and `zsched` each rewrite their whole file to add one
//! entry: load every entry, push one, save the list. Two of those in flight at
//! once and the later save writes back a list that never contained the earlier
//! writer's entry — which is gone, while both commands reported success.
//!
//! Measured before the fix: twelve concurrent `git zguard deny`, eleven rules on
//! disk, twelve exit-zero commands and one missing id. A dropped `deny` is a
//! policy someone believes is in force. This tool expects sixteen agents working
//! at once, so simultaneous writers are the ordinary case, not the exotic one.
//!
//! The assertion is exact — every entry present, every id distinct — so a
//! failure here is a real regression rather than a threshold. Detecting an
//! *unlocked* implementation is necessarily probabilistic, so the writer count
//! was chosen by measuring it: with the locks removed, twelve writers lost an
//! entry on two runs in three, twenty-four lost one on three runs in three.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");
const WRITERS: usize = 24;

fn run(home: &Path, dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap()
}

fn ok(home: &Path, dir: &Path, args: &[&str]) -> String {
    let out = run(home, dir, args);
    assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Spawn `WRITERS` processes that each add one entry, and wait for all of them.
/// Every one must report success — a writer that noticed the contention and gave
/// up would be a different bug, and the caller would at least have been told.
fn add_concurrently(home: &Path, dir: &Path, args_for: impl Fn(usize) -> Vec<String>) {
    let mut kids = Vec::new();
    for i in 0..WRITERS {
        let child = Command::new(BIN)
            .args(args_for(i))
            .current_dir(dir)
            .env("HOME", home)
            .env("ZVCS_HOME", home)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .spawn()
            .expect("spawn writer");
        kids.push(child);
    }
    for (i, mut k) in kids.into_iter().enumerate() {
        let status = k.wait().expect("writer exited");
        assert!(status.success(), "writer {i} failed: {status:?}");
    }
}

fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-regconc-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    ok(&home, &repo, &["init", "-q", "-b", "main"]);
    ok(&home, &repo, &["config", "user.email", "t@example"]);
    ok(&home, &repo, &["config", "user.name", "T"]);
    ok(&home, &repo, &["commit", "-q", "--allow-empty", "-m", "c0"]);
    (root, home, repo)
}

#[test]
fn concurrent_zguard_rules_all_survive_with_distinct_ids() {
    let (root, home, repo) = fixture("guard");

    add_concurrently(&home, &repo, |i| {
        vec!["zguard".into(), "deny".into(), format!("p{i}*")]
    });

    let listing = ok(&home, &repo, &["zguard", "list"]);
    let kept = listing.lines().filter(|l| l.contains("deny")).count();
    assert_eq!(kept, WRITERS, "a concurrent writer's rule was lost:\n{listing}");

    // Every pattern is there, and no id names two rules — the id is how a rule
    // is removed later.
    for i in 0..WRITERS {
        assert!(listing.contains(&format!("p{i}*")), "rule p{i}* was lost:\n{listing}");
    }
    let mut ids: Vec<&str> = listing
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|t| t.starts_with('#'))
        .collect();
    ids.sort_unstable();
    let total = ids.len();
    ids.dedup();
    assert_eq!(ids.len(), total, "two rules share an id:\n{listing}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn concurrent_zintercept_registrations_all_survive() {
    let (root, home, repo) = fixture("intercept");

    add_concurrently(&home, &repo, |i| {
        vec!["zintercept".into(), "before".into(), format!("q{i}"), "true".into()]
    });

    let listing = ok(&home, &repo, &["zintercept", "list"]);
    for i in 0..WRITERS {
        assert!(listing.contains(&format!("q{i} ")), "advice q{i} was lost:\n{listing}");
    }

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn concurrent_zsched_entries_all_survive() {
    let (root, home, repo) = fixture("sched");

    add_concurrently(&home, &repo, |i| {
        vec!["zsched".into(), "add".into(), format!("{}", 60 + i), format!("cmd{i}")]
    });

    // Read the file rather than the coloured listing: this is about what was
    // persisted, not about how it prints.
    let stored = std::fs::read_to_string(home.join("schedule.tsv")).expect("schedule file");
    let kept = stored.lines().filter(|l| !l.trim().is_empty()).count();
    assert_eq!(kept, WRITERS, "a concurrent schedule was lost:\n{stored}");
    for i in 0..WRITERS {
        assert!(stored.contains(&format!("cmd{i}")), "schedule cmd{i} was lost:\n{stored}");
    }

    let _ = std::fs::remove_dir_all(&root);
}
