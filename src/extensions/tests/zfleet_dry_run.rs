//! The two fleet verbs whose safety is a *default*: `git zremote` and
//! `git zrollback`.
//!
//! Neither had an integration test, and between them they carry the most
//! destructive defaults in the superset — in opposite directions, which is the
//! first thing worth pinning:
//!
//!   git zremote set <old> <new>      rewrites every matching remote URL NOW
//!   git zremote set <old> <new> -n   previews
//!   git zrollback                    previews
//!   git zrollback --apply            `reset --hard`s every selected repo
//!
//! Get either backwards and the damage is silent and immediate: a `zremote`
//! preview that writes has already repointed every remote in the tree by the
//! time it prints, and a `zrollback` default that applies has already discarded
//! commits. A test that only reads stdout cannot tell the two apart — both
//! print the same plan — so every case here asserts the **repository state**
//! afterwards: the remote URL as git reports it, and `HEAD`.
//!
//! `zrollback`'s guards get the same treatment. It refuses a dirty worktree
//! unless `--force`, and "refuses" has to mean the reset did not happen, not
//! that a line was printed.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home.join("zvcs"))
        .env("ZVCS_SOCK", home.join("sock"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .output()
        .unwrap()
}

fn ok(out: &Output, what: &str) -> String {
    assert!(
        out.status.success(),
        "{what} failed ({}): {}{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// stdout+stderr, for the verbs that report on stderr.
fn both(out: &Output) -> String {
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    s
}

/// Two indexed repositories, each with an `origin` pointing at `old.example`.
fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-zfleet-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let work = root.join("work");
    let home = root.join("home");
    std::fs::create_dir_all(home.join("zvcs")).unwrap();
    std::fs::create_dir_all(&work).unwrap();

    for name in ["one", "two"] {
        let repo = work.join(name);
        std::fs::create_dir_all(&repo).unwrap();
        ok(&git(&repo, &home, &["init", "-q", "-b", "main", "."]), "init");
        std::fs::write(repo.join("a.txt"), b"one\n").unwrap();
        ok(&git(&repo, &home, &["add", "a.txt"]), "add");
        ok(&git(&repo, &home, &["commit", "-q", "-m", "first"]), "commit");
        ok(
            &git(&repo, &home, &["remote", "add", "origin", &format!("https://old.example/{name}.git")]),
            "remote add",
        );
    }
    let idx = both(&git(&work, &home, &["zreindex", "--sync", work.to_str().unwrap()]));
    assert!(idx.contains("indexed 2"), "both repos indexed:\n{idx}");
    let one = work.join("one");
    (work, home, one)
}

fn origin_url(repo: &Path, home: &Path) -> String {
    ok(&git(repo, home, &["remote", "get-url", "origin"]), "get-url").trim().to_string()
}

fn head(repo: &Path, home: &Path) -> String {
    ok(&git(repo, home, &["rev-parse", "HEAD"]), "rev-parse").trim().to_string()
}

#[test]
fn zremote_previews_with_dry_run_and_rewrites_without_it() {
    let (work, home, one) = fixture("remote");
    let before = origin_url(&one, &home);
    assert!(before.contains("old.example"));

    // `-n` prints the plan and changes nothing.
    let out = both(&git(&work, &home, &["zremote", "set", "old.example", "new.example", "-n"]));
    assert!(out.contains("new.example"), "the preview does not show the new URL:\n{out}");
    assert_eq!(origin_url(&one, &home), before, "a dry run rewrote the remote");

    // The long spelling behaves the same.
    let _ = git(&work, &home, &["zremote", "set", "old.example", "new.example", "--dry-run"]);
    assert_eq!(origin_url(&one, &home), before, "--dry-run rewrote the remote");

    // Without it, both repositories are rewritten.
    let out = both(&git(&work, &home, &["zremote", "set", "old.example", "new.example"]));
    assert!(out.contains("new.example"), "{out}");
    for name in ["one", "two"] {
        let url = origin_url(&work.join(name), &home);
        assert!(url.contains("new.example"), "{name} was not rewritten: {url}");
        assert!(!url.contains("old.example"), "{name} kept the old host: {url}");
    }
    // The path component survives the substring rewrite.
    assert!(origin_url(&work.join("two"), &home).ends_with("/two.git"));

    let _ = std::fs::remove_dir_all(work.parent().unwrap());
}

#[test]
fn zremote_leaves_remotes_that_do_not_match_alone() {
    let (work, home, one) = fixture("nomatch");
    let before_one = origin_url(&one, &home);
    ok(&git(&one, &home, &["remote", "add", "upstream", "https://other.example/x.git"]), "add upstream");

    let _ = git(&work, &home, &["zremote", "set", "old.example", "new.example"]);
    assert_ne!(origin_url(&one, &home), before_one, "the matching remote was not rewritten");
    assert_eq!(
        ok(&git(&one, &home, &["remote", "get-url", "upstream"]), "get-url").trim(),
        "https://other.example/x.git",
        "a non-matching remote was rewritten"
    );

    let _ = std::fs::remove_dir_all(work.parent().unwrap());
}

#[test]
fn zrollback_is_a_preview_until_apply() {
    let (work, home, one) = fixture("rollback");
    // A second commit in each repo, so there is something to roll back to.
    for name in ["one", "two"] {
        let repo = work.join(name);
        std::fs::write(repo.join("a.txt"), b"two\n").unwrap();
        ok(&git(&repo, &home, &["commit", "-qam", "second"]), "second commit");
    }
    let before = head(&one, &home);

    // Default: a plan, and nothing moves.
    let out = both(&git(&work, &home, &["zrollback"]));
    assert!(out.contains("--apply"), "the preview does not say how to execute:\n{out}");
    assert_eq!(head(&one, &home), before, "the default run rolled back");

    // With `--apply`, HEAD moves back one commit in every selected repo.
    let out = both(&git(&work, &home, &["zrollback", "--apply"]));
    assert!(!out.is_empty());
    let after = head(&one, &home);
    assert_ne!(after, before, "--apply did not roll back");
    let subject = ok(&git(&one, &home, &["log", "-1", "--format=%s"]), "log").trim().to_string();
    assert_eq!(subject, "first", "rolled back to the wrong commit");

    let _ = std::fs::remove_dir_all(work.parent().unwrap());
}

#[test]
fn zrollback_refuses_a_dirty_worktree_and_that_refusal_is_a_no_op() {
    // The guard exists so a rollback cannot discard uncommitted work. Printing
    // "skip" while resetting anyway would be the worst possible bug here, so
    // the assertion is on HEAD and on the file, not on the message.
    let (work, home, one) = fixture("dirty");
    std::fs::write(one.join("a.txt"), b"two\n").unwrap();
    ok(&git(&one, &home, &["commit", "-qam", "second"]), "second commit");
    let before = head(&one, &home);

    // Uncommitted work on top.
    std::fs::write(one.join("a.txt"), b"uncommitted\n").unwrap();
    let out = both(&git(&work, &home, &["zrollback", "--apply"]));
    assert!(out.contains("dirty") || out.contains("skip"), "no refusal reported:\n{out}");
    assert_eq!(head(&one, &home), before, "a dirty repo was rolled back anyway");
    assert_eq!(
        std::fs::read_to_string(one.join("a.txt")).unwrap(),
        "uncommitted\n",
        "the uncommitted change was discarded"
    );

    // `--force` is the documented way past the guard, and it does discard.
    let out = both(&git(&work, &home, &["zrollback", "--apply", "--force"]));
    assert!(!out.is_empty());
    assert_ne!(head(&one, &home), before, "--force did not roll back");

    let _ = std::fs::remove_dir_all(work.parent().unwrap());
}
