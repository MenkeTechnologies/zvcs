//! `worktree.guessRemote` and `git worktree add --[no-]guess-remote`.
//!
//! The setting reaches exactly one decision, in `dwim_branch()` (worktree.c:767-789):
//! the `ac < 2` arm, where `worktree add <path>` was given no `<commit-ish>`, no
//! `-b`/`-B` and no `--detach`, so the new branch is named after the path's
//! basename. With the guess on and no local branch of that name, the *start point*
//! becomes `unique_tracking_name()`'s answer — the one remote-tracking ref that
//! `refs/heads/<name>` would be fetched into — instead of `HEAD`:
//!
//! ```c
//! *new_branch = branchname;
//! if (guess_remote) {
//!         char *remote = unique_tracking_name(*new_branch, &oid, NULL);
//!         return remote;
//! }
//! ```
//!
//! Two consequences follow from that being a *start point* rather than a separate
//! tracking step: the new branch's tip is the remote's commit, and the `git branch`
//! underneath sets the remote-tracking branch up as its upstream, announcing it on
//! stdout. Both are asserted, because a port that only wrote the config or only
//! moved the tip would look right in half the observations.
//!
//! Three things this file deliberately pins as *non*-effects, since they are where
//! a too-eager implementation goes wrong:
//!
//! * `--no-guess-remote` overrides the config back off — the CLI is `OPT_BOOL`, so
//!   the negative spelling is a real value, not an absence.
//! * An ambiguous name — two remotes carrying `<name>` — is a decline, not an
//!   error: `unique_tracking_name()` returns `NULL` and the add proceeds from
//!   `HEAD`, exit 0, no upstream. Both remotes here point at the *same* commit, so
//!   nothing but the count of matching refs can make this fall back.
//! * With the guess off, the same add starts at `HEAD` and writes no branch config
//!   at all.
//!
//! Fixtures are entirely local: a "remote" repository on disk, cloned over a plain
//! filesystem path, so nothing here touches the network. Identity and dates are
//! pinned, which makes the fixtures bit-identical between this port and git 2.55.0
//! — every test that can be run on stock git is, and the two are compared byte for
//! byte on stdout, stderr, exit status and resulting object ids.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Stock git for the comparison arms; absent on Linux CI, where each test still
/// asserts the whole behavior on its own.
fn stock() -> Option<&'static str> {
    let p = "/opt/homebrew/bin/git";
    Path::new(p).exists().then_some(p)
}

/// Identity and date vars git honors above config, pinned so the fixture's commits
/// are reproducible across binaries and CI environments.
const PINNED: [&str; 6] = [
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "GIT_AUTHOR_DATE",
    "GIT_COMMITTER_DATE",
];

/// A scratch `$HOME` shared by every command in this file, outside any fixture.
/// This port writes a cache directory and a sqlite database under `$HOME`; with
/// `HOME` pointing into a repository those land in its worktree and show up in the
/// `git status` these tests read.
fn home() -> PathBuf {
    let p = std::env::temp_dir().join(format!("zvcs-wtguess-home-{}", std::process::id()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn run(bin: &str, dir: &Path, args: &[&str]) -> Output {
    let mut c = Command::new(bin);
    for v in PINNED {
        c.env_remove(v);
    }
    c.args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        // A scratch HOME outside every fixture: this port keeps its own cache and
        // sqlite db under $HOME, and pointing that at a repository would leave
        // untracked files in the very worktree these tests inspect.
        .env("HOME", home())
        .env("ZVCS_HOME", home())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "2023-01-01 00:00:00 +0000")
        .env("GIT_COMMITTER_DATE", "2023-01-01 00:00:00 +0000")
        .output()
        .unwrap()
}

fn ok(bin: &str, dir: &Path, args: &[&str]) {
    let out = run(bin, dir, args);
    assert!(out.status.success(), "{bin} {args:?} failed: {}", err(&out));
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

fn err(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn out_str(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn rev(bin: &str, dir: &Path, spec: &str) -> String {
    let out = run(bin, dir, &["rev-parse", spec]);
    assert!(out.status.success(), "rev-parse {spec}: {}", err(&out));
    out_str(&out).trim().to_string()
}

fn config(bin: &str, dir: &Path, key: &str) -> Option<String> {
    let out = run(bin, dir, &["config", "--get", key]);
    out.status.success().then(|| out_str(&out).trim().to_string())
}

/// An "upstream" repository with `main` at `base` and `topic` one commit ahead,
/// cloned over a filesystem path into `dn`. In the clone, `refs/remotes/origin/topic`
/// therefore exists and names a *different* commit than `HEAD` — the whole point,
/// since a guess that silently kept starting from `HEAD` would otherwise be
/// indistinguishable. No local `topic` branch exists, which is the other condition
/// `dwim_branch()` requires before it consults a remote at all.
///
/// Returns `(root, clone)`; worktrees are added as siblings under `root`.
fn fixture(bin: &str, tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-wtguess-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();

    let up = root.join("up");
    std::fs::create_dir_all(&up).unwrap();
    ok(bin, &up, &["init", "-q", "-b", "main", "."]);
    std::fs::write(up.join("f"), "a\n").unwrap();
    ok(bin, &up, &["add", "f"]);
    ok(bin, &up, &["commit", "-q", "-m", "base"]);
    ok(bin, &up, &["checkout", "-q", "-b", "topic"]);
    std::fs::write(up.join("t"), "t\n").unwrap();
    ok(bin, &up, &["add", "t"]);
    ok(bin, &up, &["commit", "-q", "-m", "topic"]);
    ok(bin, &up, &["checkout", "-q", "main"]);

    let dn = root.join("dn");
    ok(bin, &root, &["clone", "-q", up.to_str().unwrap(), dn.to_str().unwrap()]);
    // The premise every assertion below rests on.
    assert_ne!(
        rev(bin, &dn, "HEAD"),
        rev(bin, &dn, "refs/remotes/origin/topic"),
        "fixture is not discriminating: the remote branch is at HEAD"
    );
    assert!(
        run(bin, &dn, &["rev-parse", "--verify", "-q", "refs/heads/topic"]).status.code()
            != Some(0),
        "fixture already has a local 'topic'; the DWIM would never reach a remote"
    );
    (root, dn)
}

/// Run the same scenario on stock git, in its own copy of the fixture, and return
/// its `(root, clone, output)`. `None` when stock is not installed.
fn stock_fixture(tag: &str) -> Option<(PathBuf, PathBuf, &'static str)> {
    let bin = stock()?;
    let (root, dn) = fixture(bin, &format!("stock-{tag}"));
    Some((root, dn, bin))
}

/// The `git worktree add <root>/topic` argv, with whatever `-c`/flags precede it.
fn add_argv<'a>(prefix: &[&'a str], path: &'a str) -> Vec<&'a str> {
    let mut v = prefix.to_vec();
    v.extend_from_slice(&["worktree", "add", path]);
    v
}

/// Guess off (the default): the DWIM branch starts at `HEAD`, no upstream is
/// recorded, and stdout is only the checkout's own line.
#[test]
fn without_the_guess_the_dwim_branch_starts_at_head() {
    let (root, dn) = fixture(BIN, "off");
    let wt = root.join("topic");
    let head = rev(BIN, &dn, "HEAD");
    let out = run(BIN, &dn, &add_argv(&[], wt.to_str().unwrap()));

    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(err(&out), "Preparing worktree (new branch 'topic')\n");
    assert!(!out_str(&out).contains("set up to track"), "stdout: {}", out_str(&out));
    assert_eq!(rev(BIN, &dn, "refs/heads/topic"), head);
    assert_eq!(config(BIN, &dn, "branch.topic.remote"), None);
    assert_eq!(config(BIN, &dn, "branch.topic.merge"), None);

    if let Some((sroot, sdn, sbin)) = stock_fixture("off") {
        let swt = sroot.join("topic");
        let sout = run(sbin, &sdn, &add_argv(&[], swt.to_str().unwrap()));
        assert_eq!(code(&sout), code(&out));
        assert_eq!(err(&sout), err(&out), "stderr differs from stock");
        assert_eq!(out_str(&sout), out_str(&out), "stdout differs from stock");
        assert_eq!(rev(sbin, &sdn, "refs/heads/topic"), rev(BIN, &dn, "refs/heads/topic"));
    }
}

/// `worktree.guessRemote=true`: the same command now starts the branch from
/// `refs/remotes/origin/topic` and records it as the upstream, which the child
/// `git branch` announces on stdout above the checkout's line.
#[test]
fn the_config_starts_the_dwim_branch_from_the_unique_remote_tracking_branch() {
    let (root, dn) = fixture(BIN, "cfg");
    let wt = root.join("topic");
    let head = rev(BIN, &dn, "HEAD");
    let remote_tip = rev(BIN, &dn, "refs/remotes/origin/topic");
    let argv = add_argv(&["-c", "worktree.guessRemote=true"], wt.to_str().unwrap());
    let out = run(BIN, &dn, &argv);

    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(err(&out), "Preparing worktree (new branch 'topic')\n");
    assert!(
        out_str(&out).starts_with("branch 'topic' set up to track 'origin/topic'.\n"),
        "stdout: {}",
        out_str(&out)
    );
    assert_eq!(rev(BIN, &dn, "refs/heads/topic"), remote_tip, "the branch starts at the remote tip");
    assert_ne!(rev(BIN, &dn, "refs/heads/topic"), head);
    assert_eq!(config(BIN, &dn, "branch.topic.remote").as_deref(), Some("origin"));
    assert_eq!(config(BIN, &dn, "branch.topic.merge").as_deref(), Some("refs/heads/topic"));
    // The worktree itself is checked out at the guessed start point, not at HEAD.
    assert_eq!(rev(BIN, &wt, "HEAD"), remote_tip);

    if let Some((sroot, sdn, sbin)) = stock_fixture("cfg") {
        let swt = sroot.join("topic");
        let sout = run(sbin, &sdn, &add_argv(&["-c", "worktree.guessRemote=true"], swt.to_str().unwrap()));
        assert_eq!(code(&sout), code(&out));
        assert_eq!(err(&sout), err(&out), "stderr differs from stock");
        assert_eq!(out_str(&sout), out_str(&out), "stdout differs from stock");
        assert_eq!(rev(sbin, &sdn, "refs/heads/topic"), rev(BIN, &dn, "refs/heads/topic"));
        assert_eq!(
            config(sbin, &sdn, "branch.topic.merge"),
            config(BIN, &dn, "branch.topic.merge")
        );
    }
}

/// `--guess-remote` on the command line, with no config at all, is the same
/// decision — the config seeds the variable the flag then overwrites, so both must
/// reach it.
#[test]
fn the_flag_alone_guesses_the_same_way() {
    let (root, dn) = fixture(BIN, "flag");
    let wt = root.join("topic");
    let remote_tip = rev(BIN, &dn, "refs/remotes/origin/topic");
    let out = run(BIN, &dn, &["worktree", "add", "--guess-remote", wt.to_str().unwrap()]);

    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(rev(BIN, &dn, "refs/heads/topic"), remote_tip);
    assert_eq!(config(BIN, &dn, "branch.topic.remote").as_deref(), Some("origin"));

    if let Some((sroot, sdn, sbin)) = stock_fixture("flag") {
        let swt = sroot.join("topic");
        let sout = run(sbin, &sdn, &["worktree", "add", "--guess-remote", swt.to_str().unwrap()]);
        assert_eq!(code(&sout), code(&out));
        assert_eq!(out_str(&sout), out_str(&out), "stdout differs from stock");
        assert_eq!(err(&sout), err(&out), "stderr differs from stock");
        assert_eq!(rev(sbin, &sdn, "refs/heads/topic"), rev(BIN, &dn, "refs/heads/topic"));
    }
}

/// `--no-guess-remote` beats `worktree.guessRemote=true`: the branch is back at
/// `HEAD` with no upstream, and nothing is announced.
#[test]
fn no_guess_remote_overrides_the_config() {
    let (root, dn) = fixture(BIN, "noguess");
    let wt = root.join("topic");
    let head = rev(BIN, &dn, "HEAD");
    let argv = [
        "-c",
        "worktree.guessRemote=true",
        "worktree",
        "add",
        "--no-guess-remote",
        wt.to_str().unwrap(),
    ];
    let out = run(BIN, &dn, &argv);

    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(err(&out), "Preparing worktree (new branch 'topic')\n");
    assert!(!out_str(&out).contains("set up to track"), "stdout: {}", out_str(&out));
    assert_eq!(rev(BIN, &dn, "refs/heads/topic"), head);
    assert_eq!(config(BIN, &dn, "branch.topic.merge"), None);

    if let Some((sroot, sdn, sbin)) = stock_fixture("noguess") {
        let swt = sroot.join("topic");
        let sout = run(
            sbin,
            &sdn,
            &[
                "-c",
                "worktree.guessRemote=true",
                "worktree",
                "add",
                "--no-guess-remote",
                swt.to_str().unwrap(),
            ],
        );
        assert_eq!(code(&sout), code(&out));
        assert_eq!(out_str(&sout), out_str(&out), "stdout differs from stock");
        assert_eq!(err(&sout), err(&out), "stderr differs from stock");
        assert_eq!(rev(sbin, &sdn, "refs/heads/topic"), rev(BIN, &dn, "refs/heads/topic"));
    }
}

/// Two remotes carrying `topic` make `unique_tracking_name()` decline. Both point
/// at the same commit, so only the *number* of matching remote-tracking refs can
/// explain the fallback — and the fallback is silent success from `HEAD`, not an
/// "ambiguous" diagnostic.
#[test]
fn two_remotes_carrying_the_name_decline_and_fall_back_to_head() {
    let (root, dn) = fixture(BIN, "ambig");
    let up = root.join("up");
    ok(BIN, &dn, &["remote", "add", "other", up.to_str().unwrap()]);
    ok(BIN, &dn, &["fetch", "-q", "other"]);
    assert_eq!(
        rev(BIN, &dn, "refs/remotes/origin/topic"),
        rev(BIN, &dn, "refs/remotes/other/topic"),
        "both remotes must carry the same commit, so only the count can matter"
    );

    let wt = root.join("topic");
    let head = rev(BIN, &dn, "HEAD");
    let argv = add_argv(&["-c", "worktree.guessRemote=true"], wt.to_str().unwrap());
    let out = run(BIN, &dn, &argv);

    assert_eq!(code(&out), 0, "ambiguity is a decline, not an error: {}", err(&out));
    assert_eq!(err(&out), "Preparing worktree (new branch 'topic')\n");
    assert!(!out_str(&out).contains("set up to track"), "stdout: {}", out_str(&out));
    assert_eq!(rev(BIN, &dn, "refs/heads/topic"), head);
    assert_eq!(config(BIN, &dn, "branch.topic.merge"), None);
    assert_eq!(config(BIN, &dn, "branch.topic.remote"), None);

    if let Some((sroot, sdn, sbin)) = stock_fixture("ambig") {
        let sup = sroot.join("up");
        ok(sbin, &sdn, &["remote", "add", "other", sup.to_str().unwrap()]);
        ok(sbin, &sdn, &["fetch", "-q", "other"]);
        let swt = sroot.join("topic");
        let sout = run(sbin, &sdn, &add_argv(&["-c", "worktree.guessRemote=true"], swt.to_str().unwrap()));
        assert_eq!(code(&sout), code(&out));
        assert_eq!(out_str(&sout), out_str(&out), "stdout differs from stock");
        assert_eq!(err(&sout), err(&out), "stderr differs from stock");
        assert_eq!(rev(sbin, &sdn, "refs/heads/topic"), rev(BIN, &dn, "refs/heads/topic"));
        assert_eq!(config(sbin, &sdn, "branch.topic.merge"), config(BIN, &dn, "branch.topic.merge"));
    }
}

/// A local branch of that name wins outright: `dwim_branch()` only consults a
/// remote when `refs/heads/<name>` does not resolve, so with `topic` already a
/// local branch the guess must not fire — whatever else the add then does, it must
/// not move that branch onto the remote's commit or give it an upstream.
///
/// Only that much is asserted, because the surrounding behavior currently
/// diverges and pinning it here would cement the divergence: git 2.55.0 treats the
/// existing branch as the commit-ish and checks it out
/// (`Preparing worktree (checking out 'topic')`, exit 0), while this port takes
/// the new-branch arm and dies with `fatal: a branch named 'topic' already exists`
/// (exit 255). That is `dwim_branch()`'s `branch_exists` arm, not the
/// `worktree.guessRemote` decision this file is about; measured on both binaries
/// on this same fixture.
#[test]
fn an_existing_local_branch_of_that_name_is_never_moved_by_the_guess() {
    let (root, dn) = fixture(BIN, "local");
    let head = rev(BIN, &dn, "HEAD");
    let remote_tip = rev(BIN, &dn, "refs/remotes/origin/topic");
    ok(BIN, &dn, &["branch", "topic", "HEAD"]);
    let wt = root.join("topic");
    let _ = run(BIN, &dn, &add_argv(&["-c", "worktree.guessRemote=true"], wt.to_str().unwrap()));

    assert_eq!(rev(BIN, &dn, "refs/heads/topic"), head, "the existing branch must not be moved");
    assert_ne!(rev(BIN, &dn, "refs/heads/topic"), remote_tip);
    assert_eq!(config(BIN, &dn, "branch.topic.merge"), None, "no upstream may be recorded");
    assert_eq!(config(BIN, &dn, "branch.topic.remote"), None);

    if let Some((sroot, sdn, sbin)) = stock_fixture("local") {
        ok(sbin, &sdn, &["branch", "topic", "HEAD"]);
        let swt = sroot.join("topic");
        let _ = run(sbin, &sdn, &add_argv(&["-c", "worktree.guessRemote=true"], swt.to_str().unwrap()));
        assert_eq!(
            rev(sbin, &sdn, "refs/heads/topic"),
            rev(BIN, &dn, "refs/heads/topic"),
            "stock leaves the existing branch where it was too"
        );
        assert_eq!(config(sbin, &sdn, "branch.topic.merge"), config(BIN, &dn, "branch.topic.merge"));
    }
}

/// The guess is confined to the `ac < 2` arm: with an explicit `<commit-ish>` the
/// start point is that argument, and with `-b` the branch name came from the
/// command line — neither consults a remote, config on or not. Both paths below
/// keep the worktree path's basename at `topic`, so the guess would have fired if
/// the extra argument had not taken the decision away.
#[test]
fn the_guess_cannot_move_an_explicit_commit_ish_or_a_named_branch() {
    // Explicit `<commit-ish>`: the base commit by id, which detaches. `main`
    // itself cannot be used here — it is checked out in the main worktree, and
    // that refusal would mask what this is testing.
    let (root, dn) = fixture(BIN, "explicit");
    let head = rev(BIN, &dn, "HEAD");
    let remote_tip = rev(BIN, &dn, "refs/remotes/origin/topic");
    let wt = root.join("topic");
    let out = run(
        BIN,
        &dn,
        &["-c", "worktree.guessRemote=true", "worktree", "add", wt.to_str().unwrap(), &head],
    );
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert!(
        err(&out).starts_with("Preparing worktree (detached HEAD "),
        "stderr: {}",
        err(&out)
    );
    assert_eq!(rev(BIN, &wt, "HEAD"), head);
    assert_ne!(rev(BIN, &wt, "HEAD"), remote_tip);
    assert!(
        run(BIN, &dn, &["rev-parse", "--verify", "-q", "refs/heads/topic"]).status.code()
            != Some(0),
        "an explicit commit-ish creates no branch at all"
    );

    // `-b <name>`: the branch is named on the command line, so `dwim_branch()` is
    // never reached even though `origin/topic` exists and matches the name.
    let (root2, dn2) = fixture(BIN, "explicit-b");
    let head2 = rev(BIN, &dn2, "HEAD");
    let wt2 = root2.join("topic");
    let out2 = run(
        BIN,
        &dn2,
        &["-c", "worktree.guessRemote=true", "worktree", "add", "-b", "topic", wt2.to_str().unwrap()],
    );
    assert_eq!(code(&out2), 0, "{}", err(&out2));
    assert_eq!(err(&out2), "Preparing worktree (new branch 'topic')\n");
    assert_eq!(rev(BIN, &dn2, "refs/heads/topic"), head2, "-b starts from HEAD");
    assert_eq!(config(BIN, &dn2, "branch.topic.merge"), None);
}
