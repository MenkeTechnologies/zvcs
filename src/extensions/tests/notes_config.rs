//! `git notes merge` picks its conflict strategy from config when no
//! `-s/--strategy` is given: `notes.<name>.mergeStrategy` (where `<name>` is the
//! notes ref minus its `refs/notes/` prefix) is consulted first, then the
//! general `notes.mergeStrategy`, and either overrides git's `manual` default —
//! `builtin/notes.c:merge()`.
//!
//! Each fixture is a one-commit repository carrying two conflicting notes for
//! HEAD: `AAA` on the local `refs/notes/commits` and `BBB` on `refs/notes/other`.
//! Merging `other` into `commits` forces a conflict, so the chosen strategy is
//! directly observable in the resulting note. Every case is run against the
//! system `git` (2.55.0) under a byte-identical, isolated environment and
//! compared on stdout, stderr, exit code and the merged note content.
//!
//! The one place the comparison is not full-stderr is a rejected config value:
//! git's `git_die_config()` appends the config source (` in file <path> at line
//! <n>`), but gix records no per-value line number, so the port matches git's
//! `error:` line and exit code while its `fatal:` origin clause is checked only
//! for the key name — the same substrate limitation the crate's other
//! config-fatal paths carry.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_git");

static SEQ: AtomicU64 = AtomicU64::new(0);

/// A command carrying the deterministic, isolated environment shared by the
/// fixture builder and the run under test, so `git` and the port are directly
/// comparable.
fn env_cmd(bin: &str, repo: &Path, home: &Path) -> Command {
    let mut c = Command::new(bin);
    c.current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("ZVCS_HOME", home)
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@e")
        .env("GIT_AUTHOR_DATE", "2005-04-07T22:13:13 +0000")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@e")
        .env("GIT_COMMITTER_DATE", "2005-04-07T22:13:13 +0000");
    c
}

/// Run a system-`git` command in the fixture, asserting success. Used only to
/// build the fixture and write config, never as the behavior under test.
fn git(repo: &Path, home: &Path, args: &[&str]) {
    let ok = env_cmd("git", repo, home).args(args).status().unwrap().success();
    assert!(ok, "git {args:?} failed");
}

/// A fresh fixture: a one-commit repo with two conflicting notes for HEAD, plus
/// an isolated empty `HOME` so no ambient `notes.*` config leaks in.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let uniq = SEQ.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("zvcs-notescfg-{tag}-{}-{uniq}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();

    git(&repo, &home, &["init", "-q", "-b", "main"]);
    git(&repo, &home, &["commit", "-q", "--allow-empty", "-m", "c0"]);
    // Local note on the default ref, remote note on the ref to be merged in;
    // the two disagree, so any real merge must resolve a conflict.
    git(&repo, &home, &["notes", "--ref=commits", "add", "-m", "AAA", "HEAD"]);
    git(&repo, &home, &["notes", "--ref=other", "add", "-m", "BBB", "HEAD"]);
    (repo, home)
}

/// A merge run and its aftermath: the process output and the resulting note on
/// `refs/notes/commits` (read with system `git`, so it reflects whatever the
/// binary under test wrote — or left untouched).
struct Res {
    out: Output,
    note: String,
}

/// Build a fresh fixture, apply `config`, then run `<bin> notes merge <args>`.
fn scenario(bin: &str, tag: &str, config: &[(&str, &str)], args: &[&str]) -> Res {
    let (repo, home) = fixture(tag);
    for (k, v) in config {
        git(&repo, &home, &["config", k, v]);
    }
    let mut merge_args = vec!["notes", "merge"];
    merge_args.extend_from_slice(args);
    let out = env_cmd(bin, &repo, &home).args(&merge_args).output().unwrap();
    let show = env_cmd("git", &repo, &home)
        .args(&["notes", "--ref=commits", "show", "HEAD"])
        .output()
        .unwrap();
    let note = String::from_utf8_lossy(&show.stdout).into_owned();
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    Res { out, note }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// Assert the port and system git agree on every observable of the run.
fn assert_matches_git(z: &Res, g: &Res, what: &str) {
    assert_eq!(z.out.status.code(), g.out.status.code(), "{what}: exit code");
    assert_eq!(stdout(&z.out), stdout(&g.out), "{what}: stdout");
    assert_eq!(stderr(&z.out), stderr(&g.out), "{what}: stderr");
    assert_eq!(z.note, g.note, "{what}: resulting note");
}

#[test]
fn merge_strategy_config_matches_git_and_resolves() {
    // Each auto-resolving strategy: byte-for-byte with git, and the merged note
    // is exactly what that strategy produces from `AAA` (local) and `BBB`
    // (remote).
    let expected = [
        ("ours", "AAA\n"),
        ("theirs", "BBB\n"),
        ("union", "AAA\n\nBBB\n"),
        ("cat_sort_uniq", "AAA\nBBB\n"),
    ];
    for (strat, note) in expected {
        let cfg = &[("notes.mergeStrategy", strat)];
        let z = scenario(BIN, &format!("cfg-{strat}"), cfg, &["other"]);
        let g = scenario("git", &format!("cfg-{strat}-g"), cfg, &["other"]);
        assert_eq!(z.out.status.code(), Some(0), "{strat}: clean merge:\n{}", stderr(&z.out));
        assert_eq!(z.note, note, "notes.mergeStrategy={strat} resolves to {note:?}");
        assert_matches_git(&z, &g, &format!("notes.mergeStrategy={strat}"));
    }
}

#[test]
fn cli_strategy_overrides_config() {
    // `-s` beats the config value entirely.
    let cfg = &[("notes.mergeStrategy", "ours")];
    let z = scenario(BIN, "ovr", cfg, &["-s", "theirs", "other"]);
    let g = scenario("git", "ovr-g", cfg, &["-s", "theirs", "other"]);
    assert_eq!(z.note, "BBB\n", "-s theirs must win over notes.mergeStrategy=ours");
    assert_matches_git(&z, &g, "-s overrides config");
}

#[test]
fn per_ref_strategy_overrides_general() {
    // `notes.commits.mergeStrategy` outranks the general `notes.mergeStrategy`
    // when merging into refs/notes/commits.
    let cfg = &[
        ("notes.mergeStrategy", "ours"),
        ("notes.commits.mergeStrategy", "theirs"),
    ];
    let z = scenario(BIN, "perref", cfg, &["other"]);
    let g = scenario("git", "perref-g", cfg, &["other"]);
    assert_eq!(z.note, "BBB\n", "per-ref theirs must beat general ours");
    assert_matches_git(&z, &g, "per-ref overrides general");
}

#[test]
fn unrelated_per_ref_strategy_is_ignored() {
    // A per-ref key for a *different* ref must not affect this merge; the
    // general key still applies.
    let cfg = &[
        ("notes.mergeStrategy", "ours"),
        ("notes.other.mergeStrategy", "theirs"),
    ];
    let z = scenario(BIN, "unrel", cfg, &["other"]);
    let g = scenario("git", "unrel-g", cfg, &["other"]);
    assert_eq!(z.note, "AAA\n", "notes.other.* is irrelevant to a merge into commits");
    assert_matches_git(&z, &g, "unrelated per-ref ignored");
}

#[test]
fn absent_config_defaults_to_manual() {
    // No config resolves to `manual`, which conflicts (exit 1) and leaves the
    // local note in place — identical to an explicit `-s manual`.
    let base = scenario(BIN, "man0", &[], &["other"]);
    let explicit = scenario(BIN, "man1", &[], &["-s", "manual", "other"]);
    assert_eq!(base.out.status.code(), Some(1), "manual default must conflict");
    assert_eq!(base.note, "AAA\n", "a conflicted merge keeps the local note");
    assert_eq!(stdout(&base.out), stdout(&explicit.out), "default == -s manual (stdout)");
    assert_eq!(stderr(&base.out), stderr(&explicit.out), "default == -s manual (stderr)");
    // Sanity: system git also treats an absent config as manual (conflict).
    let g = scenario("git", "man-g", &[], &["other"]);
    assert_eq!(g.out.status.code(), Some(1), "sanity: git conflicts too");
}

#[test]
fn abort_does_not_read_merge_strategy_config() {
    // git consults the strategy config only in the real-merge path, after the
    // `--abort` early-out, so a bad `notes.mergeStrategy` cannot make `--abort`
    // fail on config. The port matches git byte-for-byte here.
    let cfg = &[("notes.mergeStrategy", "bogus")];
    let z = scenario(BIN, "abort", cfg, &["--abort"]);
    let g = scenario("git", "abort-g", cfg, &["--abort"]);
    assert_matches_git(&z, &g, "--abort ignores merge-strategy config");
}

#[test]
fn invalid_merge_strategy_config_is_fatal() {
    // A present-but-unrecognised value is fatal (exit 128) and writes nothing.
    // git's leading diagnostic is reproduced verbatim; the trailing config
    // source clause (file path + line) is beyond gix's metadata, so only the
    // error line is matched against git.
    let cfg = &[("notes.mergeStrategy", "bogus")];
    let z = scenario(BIN, "bad", cfg, &["other"]);
    let g = scenario("git", "bad-g", cfg, &["other"]);
    assert_eq!(z.out.status.code(), Some(128), "invalid strategy must be fatal");
    assert_eq!(g.out.status.code(), Some(128), "sanity: git exits 128");
    let zs = stderr(&z.out);
    assert_eq!(
        zs.lines().next(),
        Some("error: unknown notes merge strategy bogus"),
        "leading diagnostic:\n{zs}"
    );
    assert_eq!(zs.lines().next(), stderr(&g.out).lines().next(), "error line matches git");
    assert!(
        zs.contains("bad config variable 'notes.mergeStrategy'"),
        "fatal names the offending key:\n{zs}"
    );
    assert_eq!(z.note, "AAA\n", "a rejected strategy writes no note");
}

#[test]
fn invalid_per_ref_strategy_config_is_fatal_and_names_the_key() {
    // The per-ref key is validated the same way, and its full name appears in
    // the fatal.
    let cfg = &[("notes.commits.mergeStrategy", "nope")];
    let z = scenario(BIN, "badref", cfg, &["other"]);
    assert_eq!(z.out.status.code(), Some(128), "invalid per-ref strategy must be fatal");
    let zs = stderr(&z.out);
    assert_eq!(
        zs.lines().next(),
        Some("error: unknown notes merge strategy nope"),
        "leading diagnostic:\n{zs}"
    );
    assert!(
        zs.contains("bad config variable 'notes.commits.mergeStrategy'"),
        "fatal names the per-ref key:\n{zs}"
    );
    assert_eq!(z.note, "AAA\n", "a rejected per-ref strategy writes no note");
}
