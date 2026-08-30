//! `git zguard`'s `--when` predicates and its pattern matcher.
//!
//! `zguard_policy.rs` covers the machinery — deny stops, warn allows, rules are
//! listed and removed — with one predicate (`protected`). The other three
//! (`detached`, `dirty`, `unsigned`) decide whether a rule applies at all, so a
//! predicate that never holds is a policy the user believes is in force and
//! isn't. Each is asserted in both directions: the rule fires when the state
//! holds and stays out of the way when it does not.
//!
//! The matcher is pinned alongside, because a policy that matches too much is as
//! wrong as one that matches too little. `git zguard deny 'push*--force*'` — the
//! form the man page gives — must stop a force-push and leave a plain push
//! alone. Note that patterns are globs against the whole command line: a
//! wildcard-free `push --force` matches nothing at all, which is measured here
//! so it cannot change unnoticed.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
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

fn ok(out: &Output, what: &str) -> String {
    assert!(out.status.success(), "{what} failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The verdict `git zguard test` reports for a command, without running it.
fn verdict(repo: &Path, home: &Path, cmd: &[&str]) -> String {
    let mut args = vec!["zguard", "test"];
    args.extend_from_slice(cmd);
    let out = run(repo, home, &args);
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    s.lines().next().unwrap_or_default().split_whitespace().next().unwrap_or_default().to_string()
}

fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-guardp-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    ok(&run(&repo, &home, &["init", "-q", "-b", "main"]), "init");
    ok(&run(&repo, &home, &["config", "user.email", "t@example"]), "email");
    ok(&run(&repo, &home, &["config", "user.name", "T"]), "name");
    std::fs::write(repo.join("f.txt"), b"a\n").unwrap();
    ok(&run(&repo, &home, &["add", "f.txt"]), "add");
    ok(&run(&repo, &home, &["commit", "-q", "-m", "c0"]), "commit");
    (root, home)
}

fn head(repo: &Path, home: &Path) -> String {
    ok(&run(repo, home, &["rev-parse", "HEAD"]), "rev-parse").trim().to_string()
}

#[test]
fn the_detached_predicate_follows_head() {
    let (root, home) = fixture("detached");
    let repo = root.join("repo");
    ok(&run(&repo, &home, &["zguard", "deny", "commit*", "--when", "detached"]), "deny");

    // On a branch the rule is inert, and the command really runs.
    assert_eq!(verdict(&repo, &home, &["commit", "-m", "x"]), "allow");
    let before = head(&repo, &home);
    std::fs::write(repo.join("f.txt"), b"b\n").unwrap();
    assert!(run(&repo, &home, &["commit", "-qam", "on a branch"]).status.success(), "attached commit must run");
    assert_ne!(head(&repo, &home), before, "the commit must have happened");

    // Detached, the same command is refused and nothing moves.
    ok(&run(&repo, &home, &["checkout", "-q", "--detach"]), "detach");
    assert_eq!(verdict(&repo, &home, &["commit", "-m", "x"]), "DENY");
    let detached_head = head(&repo, &home);
    std::fs::write(repo.join("f.txt"), b"c\n").unwrap();
    let blocked = run(&repo, &home, &["commit", "-qam", "while detached"]);
    assert!(!blocked.status.success(), "a denied commit must fail");
    assert_eq!(head(&repo, &home), detached_head, "a denied commit must not land");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_dirty_predicate_follows_the_worktree() {
    let (root, home) = fixture("dirty");
    let repo = root.join("repo");
    ok(&run(&repo, &home, &["zguard", "deny", "status*", "--when", "dirty"]), "deny");

    // Clean: inert.
    assert_eq!(verdict(&repo, &home, &["status"]), "allow");
    assert!(run(&repo, &home, &["status", "--porcelain"]).status.success(), "clean status must run");

    // A tracked modification makes the predicate hold, and the command is refused.
    std::fs::write(repo.join("f.txt"), b"modified\n").unwrap();
    assert_eq!(verdict(&repo, &home, &["status"]), "DENY");
    assert!(!run(&repo, &home, &["status", "--porcelain"]).status.success(), "a denied command must fail");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_unsigned_predicate_reads_the_command_and_the_config() {
    let (root, home) = fixture("unsigned");
    let repo = root.join("repo");
    ok(&run(&repo, &home, &["zguard", "deny", "commit*", "--when", "unsigned"]), "deny");

    // A plain commit is unsigned: refused, and it does not land.
    assert_eq!(verdict(&repo, &home, &["commit", "-m", "x"]), "DENY");
    let before = head(&repo, &home);
    std::fs::write(repo.join("f.txt"), b"b\n").unwrap();
    assert!(!run(&repo, &home, &["commit", "-qam", "unsigned"]).status.success(), "an unsigned commit must be refused");
    assert_eq!(head(&repo, &home), before, "the refused commit must not land");

    // Asking for a signature clears the predicate. Checked through `zguard test`
    // rather than a real commit: the predicate reads the request (`-S`, or
    // commit.gpgsign), not a signature it verified, so this needs no signer.
    assert_eq!(verdict(&repo, &home, &["commit", "-S", "-m", "x"]), "allow", "-S must clear `unsigned`");
    ok(&run(&repo, &home, &["config", "commit.gpgsign", "true"]), "gpgsign");
    assert_eq!(verdict(&repo, &home, &["commit", "-m", "x"]), "allow", "commit.gpgsign must clear `unsigned`");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_pattern_matches_the_whole_command_line_as_a_glob() {
    let (root, home) = fixture("pattern");
    let repo = root.join("repo");

    // The man page's own example: a force-push is refused, a plain push is not.
    // A policy that catches both would be as wrong as one that catches neither.
    ok(&run(&repo, &home, &["zguard", "deny", "push*--force*"]), "deny force");
    assert_eq!(verdict(&repo, &home, &["push", "--force", "origin", "main"]), "DENY");
    assert_eq!(verdict(&repo, &home, &["push", "origin", "main"]), "allow");
    assert_eq!(verdict(&repo, &home, &["fetch", "--force"]), "allow", "the verb is part of the match");
    ok(&run(&repo, &home, &["zguard", "clear"]), "clear");

    // A bare verb name matches that verb whatever its arguments.
    ok(&run(&repo, &home, &["zguard", "deny", "push"]), "deny push");
    assert_eq!(verdict(&repo, &home, &["push", "origin", "main"]), "DENY");
    assert_eq!(verdict(&repo, &home, &["fetch"]), "allow");
    ok(&run(&repo, &home, &["zguard", "clear"]), "clear");

    // Patterns are globs, so a wildcard-free multi-word pattern can never match:
    // it is neither `*` nor a verb name. Measured, and pinned so that a change to
    // the shared matcher has to be a deliberate one.
    ok(&run(&repo, &home, &["zguard", "deny", "push --force"]), "deny literal");
    assert_eq!(
        verdict(&repo, &home, &["push", "--force"]),
        "allow",
        "a wildcard-free multi-word pattern matches nothing — the man page's form is `push*--force*`"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_policy_verbs_still_work_under_deny_everything() {
    let (root, home) = fixture("lockout");
    let repo = root.join("repo");
    ok(&run(&repo, &home, &["zguard", "deny", "*", "-m", "everything"]), "deny all");

    // Documented: the policy verbs are exempt from their own rules, so a rule
    // that denies everything can always be inspected and taken back out.
    assert_eq!(verdict(&repo, &home, &["status"]), "DENY", "the rule must be live");
    let listed = ok(&run(&repo, &home, &["zguard", "list"]), "list under deny *");
    assert!(listed.contains("deny"), "the rule must still be listable:\n{listed}");
    ok(&run(&repo, &home, &["zguard", "clear"]), "clear under deny *");
    assert_eq!(verdict(&repo, &home, &["status"]), "allow", "clearing must restore normal operation");

    let _ = std::fs::remove_dir_all(&root);
}
