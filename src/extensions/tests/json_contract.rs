//! The `--json` contract, swept across every verb that advertises it.
//!
//! `json_output.rs` parses the output of a hand-written list of verbs. Two gaps
//! follow from that: the list drifts (a third of the verbs documenting `--json`
//! were never in it), and parsing alone accepts any JSON, so a verb emitting one
//! array on one line passes while every sibling emits NDJSON — which is what
//! `zppid` and `zprocs` did.
//!
//! This sweep derives its own list from the man page at runtime, so a verb that
//! starts advertising `--json` is covered the day it does, and checks the shape
//! scripts actually depend on: every non-empty line is a JSON *object*.
//!
//! It also pins the two documentation sources against each other. The man
//! synopsis and the verb's own `-h` usage line are written by hand in different
//! files, and `-h` is what people read: 33 verbs supported `--json`, documented
//! it in the man page, and never mentioned it in `-h`.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");
const MANPAGE: &str = include_str!("../src/superset/manpage.rs");

fn run(home: &Path, cwd: &Path, args: &[&str]) -> String {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("run binary");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `(verb, synopsis)` for every documented verb, read from the man-page table
/// that `git help <verb>` and `docs/reference.html` are both generated from.
fn documented() -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut verb: Option<String> = None;
    for line in MANPAGE.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("verb: \"") {
            verb = rest.split('"').next().map(str::to_string);
        } else if let Some(rest) = t.strip_prefix("synopsis: \"") {
            if let (Some(v), Some(s)) = (verb.take(), rest.split('"').next()) {
                out.push((v, s.to_string()));
            }
        }
    }
    assert!(out.len() > 100, "the man-page table failed to parse ({} entries)", out.len());
    out
}

/// Arguments a verb needs before `--json` means anything, and the flag that
/// stops the feed verbs from following forever (they would hang the sweep).
fn extra_args(verb: &str) -> Vec<&'static str> {
    match verb {
        "zgrep" => vec!["."],
        "zsince" => vec!["1h"],
        "zpin" => vec!["list"],
        "zstatus" => vec!["--all"],
        "zevents" | "ztail" | "zcommands" => vec!["--no-follow"],
        _ => vec![],
    }
}

/// A repo with a commit, a tag, and content to grep, indexed so the fleet-wide
/// read verbs have a row to emit.
fn fixture(tag: &str) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-jsonc-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    let home = root.join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    run(&home, &repo, &["init", "-q", "-b", "main"]);
    run(&home, &repo, &["config", "user.email", "t@example"]);
    run(&home, &repo, &["config", "user.name", "T"]);
    std::fs::write(repo.join("f.txt"), "findable\n").unwrap();
    run(&home, &repo, &["add", "f.txt"]);
    run(&home, &repo, &["commit", "-q", "-m", "base"]);
    run(&home, &repo, &["tag", "v1"]);
    run(&home, &repo, &["zreindex", "--sync", repo.to_str().unwrap()]);
    (root, home, repo)
}

#[test]
fn every_verb_advertising_json_emits_ndjson_objects() {
    let (root, home, repo) = fixture("sweep");
    let advertised: Vec<String> = documented()
        .into_iter()
        .filter(|(_, syn)| syn.contains("--json"))
        .map(|(v, _)| v)
        .collect();
    assert!(advertised.len() >= 38, "expected the documented --json set, got {}", advertised.len());

    let mut with_rows = 0usize;
    for verb in &advertised {
        let mut args: Vec<&str> = vec![verb.as_str()];
        args.extend(extra_args(verb));
        args.push("--json");
        let out = run(&home, &repo, &args);
        for (i, line) in out.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let v: serde_json::Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("`git {}` line {} is not JSON: {line:?} ({e})", args.join(" "), i + 1));
            // NDJSON: one object per line. An array (or a scalar) on one line
            // parses fine but breaks every consumer that reads line by line.
            assert!(v.is_object(), "`git {}` line {} is not a JSON object: {line:?}", args.join(" "), i + 1);
        }
        if out.lines().any(|l| !l.trim().is_empty()) {
            with_rows += 1;
        }
    }

    // Guard against a sweep that passes because everything printed nothing.
    assert!(with_rows >= 10, "only {with_rows} of {} verbs produced any row", advertised.len());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_usage_line_and_the_man_page_agree_about_json() {
    let (root, home, repo) = fixture("agree");

    // `-h` and the man page are hand-written in different files; a verb that
    // supports --json must say so in both, since -h is what gets read first.
    let mut missing_from_usage = Vec::new();
    let mut missing_from_man = Vec::new();
    for (verb, synopsis) in documented() {
        let usage = run(&home, &repo, &[&verb, "-h"]);
        let first = usage.lines().next().unwrap_or_default().to_string();
        if first.is_empty() {
            continue; // verb prints its help elsewhere; the sweep above still covers it
        }
        match (synopsis.contains("--json"), first.contains("--json")) {
            (true, false) => missing_from_usage.push(verb),
            (false, true) => missing_from_man.push(verb),
            _ => {}
        }
    }
    assert!(
        missing_from_usage.is_empty(),
        "these verbs document --json in the man page but hide it from `-h`: {missing_from_usage:?}"
    );
    assert!(
        missing_from_man.is_empty(),
        "these verbs offer --json in `-h` but not in the man page: {missing_from_man:?}"
    );

    let _ = std::fs::remove_dir_all(&root);
}
