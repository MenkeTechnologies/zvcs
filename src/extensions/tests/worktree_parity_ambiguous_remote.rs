//! `git worktree add` against remote-tracking branches, as git 2.56.0 decides it.
//!
//! Two arms of `add()` (builtin/worktree.c) ask `unique_tracking_name()` which
//! remote carries a name: the `ac == 2` arm, for a `<commit-ish>` that names no
//! commit, and `dwim_branch()`, for `worktree add <path>` under
//! `worktree.guessRemote`. 2.56 changed both:
//!
//! * a name on more than one remote, with `checkout.defaultRemote` naming none of
//!   them, is refused — `advise_disambiguating_remotes()` lists the remotes and the
//!   `-b` spelling that picks one, then `'<name>' matched multiple (<n>) remote
//!   tracking branches`, exit 128 — where 2.55 fell through to `invalid reference`
//!   or silently started from `HEAD`;
//! * an explicit `-b`/`-B` keeps the `ac == 2` guess from running at all, so the
//!   name the command line gave is never replaced by the guessed one.
//!
//! The hint is skipped under `--quiet` and with
//! `advice.checkoutAmbiguousRemoteBranchName=false`; the refusal is not. The path
//! in it is `prefix_filename(prefix, <path>)`, so from a subdirectory it carries
//! the subdirectory.
//!
//! Every expectation was measured from stock git 2.56.0 on this same fixture and is
//! written as a literal: identity and dates are pinned, so the abbreviated ids in
//! the `HEAD is now at` lines are stable.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, args: &[&str]) -> Output {
    let home = std::env::temp_dir().join(format!("zvcs-wtamb-home-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .env("HOME", &home)
        .env("ZVCS_HOME", &home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@x")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@x")
        .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
        .output()
        .unwrap()
}

fn ok(dir: &Path, args: &[&str]) {
    let out = run(dir, args);
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// `(exit, stdout, stderr)`.
fn res(out: &Output) -> (i32, String, String) {
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn config(dir: &Path, key: &str) -> Option<String> {
    let out = run(dir, &["config", "--get", key]);
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn has_ref(dir: &Path, name: &str) -> bool {
    run(dir, &["rev-parse", "--verify", "-q", name]).status.success()
}

/// `r` with `c1` and `c2` on `main`, a `sub/` directory, and two configured remotes
/// that both carry `topic` (at `c1`) while only `origin` carries `solo`. The
/// remote-tracking refs are written directly, so nothing is fetched. Returns
/// `(root, repo)`; worktrees go next to `r` under `root`.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-wtamb-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let repo = root.join("r");
    ok(&root, &["init", "-q", "-b", "main", "r"]);
    std::fs::write(repo.join("f"), "1\n").unwrap();
    ok(&repo, &["add", "f"]);
    ok(&repo, &["commit", "-q", "-m", "c1"]);
    std::fs::write(repo.join("f"), "2\n").unwrap();
    ok(&repo, &["commit", "-q", "-a", "-m", "c2"]);
    for remote in ["origin", "upstream"] {
        ok(&repo, &["update-ref", &format!("refs/remotes/{remote}/topic"), "HEAD~1"]);
        ok(&repo, &["config", &format!("remote.{remote}.url"), &format!("/x/{remote}")]);
        ok(
            &repo,
            &["config", &format!("remote.{remote}.fetch"), &format!("+refs/heads/*:refs/remotes/{remote}/*")],
        );
    }
    ok(&repo, &["update-ref", "refs/remotes/origin/solo", "HEAD~1"]);
    std::fs::create_dir_all(repo.join("sub")).unwrap();
    (root, repo)
}

fn ambiguity_hint(path: &str) -> String {
    format!(
        "hint: Branch name 'topic' appears in multiple remotes:\n\
         hint:   origin\n\
         hint:   upstream\n\
         hint: If you meant to create a worktree from a remote tracking branch on\n\
         hint: <remote>, you can do so by:\n\
         hint:\n\
         hint:     git worktree add -b topic {path} <remote>/topic\n\
         hint:\n\
         hint: If you'd like to always prefer some remote, e.g. 'origin',\n\
         hint: consider setting checkout.defaultRemote=origin in your config.\n"
    )
}

const MATCHED_TWO: &str = "fatal: 'topic' matched multiple (2) remote tracking branches\n";

/// The `ac == 2` arm: `topic` names no commit and two remotes carry it. Nothing is
/// created — no branch, no worktree directory.
#[test]
fn an_explicit_commit_ish_on_two_remotes_is_refused_with_the_remotes_listed() {
    let (root, repo) = fixture("ac2");
    let out = run(&repo, &["worktree", "add", "../w1", "topic"]);
    assert_eq!(res(&out), (128, String::new(), format!("{}{MATCHED_TWO}", ambiguity_hint("../w1"))));
    assert!(!has_ref(&repo, "refs/heads/topic"));
    assert!(!root.join("w1").exists());
}

/// `--quiet` and the advice key each drop the hint; the refusal stays.
#[test]
fn quiet_and_the_advice_key_keep_only_the_refusal() {
    let (_root, repo) = fixture("quiet");
    let out = run(&repo, &["worktree", "add", "-q", "../w1", "topic"]);
    assert_eq!(res(&out), (128, String::new(), MATCHED_TWO.to_owned()));
    let out = run(
        &repo,
        &["-c", "advice.checkoutAmbiguousRemoteBranchName=false", "worktree", "add", "../w1", "topic"],
    );
    assert_eq!(res(&out), (128, String::new(), MATCHED_TWO.to_owned()));
}

/// `path` in the hint is `prefix_filename(prefix, av[0])`: typed from `sub/`, the
/// path is spelled from the top of the worktree.
#[test]
fn the_hint_path_carries_the_subdirectory_prefix() {
    let (_root, repo) = fixture("prefix");
    let out = run(&repo.join("sub"), &["worktree", "add", "../../w9", "topic"]);
    assert_eq!(
        res(&out),
        (128, String::new(), format!("{}{MATCHED_TWO}", ambiguity_hint("sub/../../w9")))
    );
}

/// `checkout.defaultRemote` settles a multi-remote match: the branch starts from that
/// remote's ref and tracks it. A value naming no matching remote settles nothing.
#[test]
fn checkout_default_remote_picks_one_of_the_matches() {
    let (root, repo) = fixture("default");
    let out = run(&repo, &["-c", "checkout.defaultRemote=upstream", "worktree", "add", "../w1", "topic"]);
    assert_eq!(
        res(&out),
        (
            0,
            "branch 'topic' set up to track 'upstream/topic'.\nHEAD is now at e84063f c1\n".to_owned(),
            "Preparing worktree (new branch 'topic')\n".to_owned()
        )
    );
    assert_eq!(config(&repo, "branch.topic.remote").as_deref(), Some("upstream"));
    assert_eq!(config(&repo, "branch.topic.merge").as_deref(), Some("refs/heads/topic"));
    assert!(root.join("w1").join("f").exists());

    let out = run(&repo, &["-c", "checkout.defaultRemote=nosuch", "worktree", "add", "../w6", "topic2"]);
    assert_eq!(res(&out).0, 128, "topic2 is on no remote: {}", res(&out).2);
    ok(&repo, &["update-ref", "refs/remotes/upstream/two", "HEAD~1"]);
    ok(&repo, &["update-ref", "refs/remotes/origin/two", "HEAD~1"]);
    let out = run(&repo, &["-c", "checkout.defaultRemote=nosuch", "worktree", "add", "-q", "../w6", "two"]);
    assert_eq!(
        res(&out),
        (128, String::new(), "fatal: 'two' matched multiple (2) remote tracking branches\n".to_owned())
    );
}

/// One remote carrying the name: `-b <name> <remote-tracking ref>`, with the
/// upstream set by the child `git branch`.
#[test]
fn an_explicit_commit_ish_on_one_remote_becomes_a_tracking_branch() {
    let (root, repo) = fixture("one");
    let out = run(&repo, &["worktree", "add", "../w5", "solo"]);
    assert_eq!(
        res(&out),
        (
            0,
            "branch 'solo' set up to track 'origin/solo'.\nHEAD is now at e84063f c1\n".to_owned(),
            "Preparing worktree (new branch 'solo')\n".to_owned()
        )
    );
    assert_eq!(config(&repo, "branch.solo.remote").as_deref(), Some("origin"));
    assert!(root.join("w5").join("f").exists());
}

/// `-b`/`-B` and `--detach` take the guess away: the `<commit-ish>` must name a
/// commit on its own. Through 2.55 `-b nb ../w3 solo` had the guess replace `nb`
/// with `solo`.
#[test]
fn a_named_branch_or_detach_never_guesses_from_a_remote() {
    let (root, repo) = fixture("named");
    for argv in [
        &["worktree", "add", "-b", "nb", "../w3", "solo"][..],
        &["worktree", "add", "-B", "nb", "../w3", "topic"][..],
        &["worktree", "add", "--detach", "../w4", "topic"][..],
    ] {
        let name = argv[argv.len() - 1];
        let out = run(&repo, argv);
        assert_eq!(
            res(&out),
            (128, String::new(), format!("fatal: invalid reference: {name}\n")),
            "{argv:?}"
        );
    }
    assert!(!has_ref(&repo, "refs/heads/nb"));
    assert!(!has_ref(&repo, "refs/heads/solo"));
    assert!(!root.join("w3").exists());
}

/// `dwim_branch()` under `worktree.guessRemote`: the same refusal, the path being
/// the one the branch name was taken from; `checkout.defaultRemote` settles it.
#[test]
fn the_guess_remote_dwim_refuses_an_ambiguous_name_too() {
    let (_root, repo) = fixture("guess");
    let out = run(&repo, &["worktree", "add", "--guess-remote", "../topic"]);
    assert_eq!(res(&out), (128, String::new(), format!("{}{MATCHED_TWO}", ambiguity_hint("../topic"))));
    assert!(!has_ref(&repo, "refs/heads/topic"));

    let out = run(
        &repo,
        &["-c", "worktree.guessRemote=true", "-c", "checkout.defaultRemote=origin", "worktree", "add", "../topic"],
    );
    assert_eq!(
        res(&out),
        (
            0,
            "branch 'topic' set up to track 'origin/topic'.\nHEAD is now at e84063f c1\n".to_owned(),
            "Preparing worktree (new branch 'topic')\n".to_owned()
        )
    );
}
