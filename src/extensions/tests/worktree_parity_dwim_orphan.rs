//! `git worktree add`'s inferred `--orphan`, and the `--reason`/`--lock` pairing.
//!
//! A repository with nothing to start a worktree from — a fresh `git init`, a
//! bare repo before its first commit — is not an error case in git. `add()` runs
//! `dwim_orphan()` (`builtin/worktree.c:746-763`) on both `ac < 2` arms, prints
//! `No possible source branch, inferring '--orphan'`, and creates the worktree on
//! an unborn branch. Four behaviours were missing or wrong here, each measured
//! against git 2.55.0 before being ported:
//!
//! * The inference itself. `git worktree add ../a` in an empty repo died with
//!   `invalid reference: HEAD` instead of creating an unborn-branch worktree.
//! * `can_use_remote_refs()` (`worktree.c:716-770`): with `--guess-remote`, no
//!   refs anywhere and a remote configured, git *stops* rather than invent an
//!   unborn branch on a repository that was probably just never fetched. The
//!   name of that remote follows `remotes_remote_for_branch()`
//!   (`remote.c:666-680`) — `branch.<head>.remote`, else the single configured
//!   remote, else `origin` — and an explicitly configured name counts even with
//!   no `remote.<name>.url`, because `remotes_remote_get_1()` turns it into a URL
//!   alias (`remote.c:816-817`).
//! * The `ac == 2` arm's `can_use_local_refs()` call (`worktree.c:910-911`):
//!   `git worktree add <path> HEAD` against a broken `HEAD` warns before it dies.
//! * `--reason` without `--lock` is `die()` at `worktree.c:848-849`, not a
//!   silently ignored option.
//!
//! Fixtures are built with the zvcs binary itself, so nothing here needs a stock
//! git on PATH, and every path is derived from the test's own temp root.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Identity and date vars git honors above config. CI exports some of these for
/// the whole job, which would change the commits these fixtures build.
const PINNED: [&str; 6] = [
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "GIT_AUTHOR_DATE",
    "GIT_COMMITTER_DATE",
];

fn git(dir: &Path, args: &[&str]) -> Output {
    let mut c = Command::new(BIN);
    for v in PINNED {
        c.env_remove(v);
    }
    c.args(args)
        .current_dir(dir)
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

fn ok(dir: &Path, args: &[&str]) {
    let out = git(dir, args);
    assert!(out.status.success(), "git {args:?} failed: {}", err(&out));
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

/// A scratch root plus an *empty* repository in it — `git init` and nothing else,
/// which is the state `dwim_orphan()` exists for.
fn empty_repo(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-wtdo-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let r = root.join("r");
    std::fs::create_dir_all(&r).unwrap();
    ok(&r, &["init", "-q", "-b", "main", "."]);
    (root, r)
}

/// The same, with one commit on `main`.
fn repo_with_commit(tag: &str) -> (PathBuf, PathBuf) {
    let (root, r) = empty_repo(tag);
    std::fs::write(r.join("f.txt"), "a\n").unwrap();
    ok(&r, &["add", "f.txt"]);
    ok(&r, &["commit", "-q", "-m", "base"]);
    (root, r)
}

/// `refs/heads/<name>` as the worktree's own `HEAD` file records it. Reading the
/// administrative file rather than asking `symbolic-ref` is deliberate: an unborn
/// branch has no object behind it, so the on-disk symref is the only evidence
/// that the worktree was created at all.
fn wt_head(repo: &Path, id: &str) -> String {
    std::fs::read_to_string(repo.join(".git/worktrees").join(id).join("HEAD"))
        .unwrap()
        .trim()
        .to_string()
}

/// The bare `worktree add <path>` arm: `dwim_branch()` finds nothing, so
/// `dwim_orphan(…, remote = 1)` infers `--orphan` and the branch is named after
/// the path (`worktree.c:893-898`).
#[test]
fn empty_repo_infers_orphan_and_names_the_branch_after_the_path() {
    let (root, r) = empty_repo("infer");
    let wt = root.join("feature");
    let out = git(&r, &["worktree", "add", wt.to_str().unwrap()]);

    assert_eq!(code(&out), 0, "{}", err(&out));
    assert!(
        err(&out).contains("No possible source branch, inferring '--orphan'"),
        "expected the inference line, got {:?}",
        err(&out)
    );
    assert_eq!(wt_head(&r, "feature"), "ref: refs/heads/feature");
    assert!(wt.join(".git").is_file(), "the worktree's .git link was not written");
}

/// The `-b` arm takes `dwim_orphan(…, remote = 0)` (`worktree.c:888-889`), so the
/// branch is the one `-b` named and `--guess-remote` cannot reach it.
#[test]
fn empty_repo_with_dash_b_infers_orphan_on_the_named_branch() {
    let (root, r) = empty_repo("infer-b");
    let wt = root.join("w");
    let out = git(&r, &["worktree", "add", "-b", "bee", wt.to_str().unwrap()]);

    assert_eq!(code(&out), 0, "{}", err(&out));
    assert!(err(&out).contains("No possible source branch, inferring '--orphan'"));
    assert_eq!(wt_head(&r, "w"), "ref: refs/heads/bee");
}

/// `--quiet` silences the inference line without changing the outcome
/// (`worktree.c:751-753`). The control half — that the same add is loud without
/// it — is the test above.
#[test]
fn quiet_suppresses_the_inference_line_but_still_infers() {
    let (root, r) = empty_repo("infer-quiet");
    let wt = root.join("w");
    let out = git(&r, &["worktree", "add", "-q", wt.to_str().unwrap()]);

    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(err(&out), "", "--quiet must emit nothing on success");
    assert_eq!(wt_head(&r, "w"), "ref: refs/heads/w");
}

/// The inferred `--orphan` is checked against the same options the explicit
/// spelling is (`worktree.c:755-761`), and the refusal names `--orphan` even
/// though the user never typed it. `--no-checkout` and `--track` are the two.
#[test]
fn inferred_orphan_refuses_no_checkout_and_track() {
    let (root, r) = empty_repo("infer-combo");

    let nc = root.join("nc");
    let out = git(&r, &["worktree", "add", "--no-checkout", nc.to_str().unwrap()]);
    assert_eq!(code(&out), 128, "{}", err(&out));
    assert!(
        err(&out).contains("No possible source branch, inferring '--orphan'")
            && err(&out)
                .contains("fatal: options '--orphan' and '--no-checkout' cannot be used together"),
        "got {:?}",
        err(&out)
    );
    assert!(!nc.exists(), "nothing may be left behind by the refusal");

    let tr = root.join("tr");
    let out = git(&r, &["worktree", "add", "-b", "x", "--track", tr.to_str().unwrap()]);
    assert_eq!(code(&out), 128, "{}", err(&out));
    assert!(
        err(&out).contains("fatal: options '--orphan' and '--track' cannot be used together"),
        "got {:?}",
        err(&out)
    );
    assert!(!tr.exists());
}

/// `can_use_remote_refs()` (`worktree.c:751-770`): `--guess-remote`, no refs
/// anywhere, but `origin` is configured — git stops and points at `-f`. The
/// message precedes the inference line, because the `die()` is inside
/// `can_use_remote_refs()` and that runs first (`worktree.c:749-750`).
#[test]
fn guess_remote_with_a_configured_remote_and_no_refs_stops() {
    let (root, r) = empty_repo("fetch-stop");
    ok(&r, &["remote", "add", "origin", "https://example.invalid/x.git"]);
    let wt = root.join("w");
    let out = git(&r, &["worktree", "add", "--guess-remote", wt.to_str().unwrap()]);

    assert_eq!(code(&out), 128, "{}", err(&out));
    assert!(
        err(&out).contains(
            "fatal: No local or remote refs exist despite at least one remote\n\
             present, stopping; use 'add -f' to override or fetch a remote first"
        ),
        "got {:?}",
        err(&out)
    );
    assert!(
        !err(&out).contains("No possible source branch"),
        "the stop happens before the inference line, got {:?}",
        err(&out)
    );
    assert!(!wt.exists());
}

/// `-f` is the documented override (`worktree.c:768`), and without
/// `--guess-remote` the check is never reached at all (`worktree.c:752-753`) —
/// so the same fixture that stops above succeeds twice over.
#[test]
fn the_fetch_stop_is_bypassed_by_force_and_by_not_guessing() {
    let (root, r) = empty_repo("fetch-stop-bypass");
    ok(&r, &["remote", "add", "origin", "https://example.invalid/x.git"]);

    let forced = root.join("forced");
    let out = git(&r, &["worktree", "add", "--guess-remote", "-f", forced.to_str().unwrap()]);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(wt_head(&r, "forced"), "ref: refs/heads/forced");

    let plain = root.join("plain");
    let out = git(&r, &["worktree", "add", plain.to_str().unwrap()]);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(wt_head(&r, "plain"), "ref: refs/heads/plain");
}

/// `remotes_remote_for_branch()` (`remote.c:666-680`) does not hardcode `origin`:
/// with exactly one remote configured, that one is the default. So a lone remote
/// under any name still trips the stop.
#[test]
fn the_sole_configured_remote_is_the_default_even_when_not_named_origin() {
    let (root, r) = empty_repo("sole-remote");
    ok(&r, &["remote", "add", "upstream", "https://example.invalid/x.git"]);
    let out = git(&r, &["worktree", "add", "--guess-remote", root.join("w").to_str().unwrap()]);

    assert_eq!(code(&out), 128, "{}", err(&out));
    assert!(err(&out).contains("No local or remote refs exist despite at least one remote"));
}

/// Two remotes and neither is `origin`: the default name falls back to `origin`,
/// which is not configured, so `remote_get(NULL)` is NULL and the inference goes
/// ahead. This is the control that the test above is measuring the *default
/// remote* rather than "any remote exists".
#[test]
fn two_non_origin_remotes_leave_no_default_so_the_inference_proceeds() {
    let (root, r) = empty_repo("two-remotes");
    ok(&r, &["remote", "add", "upstream", "https://example.invalid/x.git"]);
    ok(&r, &["remote", "add", "downstream", "https://example.invalid/y.git"]);
    let out = git(&r, &["worktree", "add", "--guess-remote", root.join("w").to_str().unwrap()]);

    assert_eq!(code(&out), 0, "{}", err(&out));
    assert!(err(&out).contains("No possible source branch, inferring '--orphan'"));
    assert_eq!(wt_head(&r, "w"), "ref: refs/heads/w");
}

/// An explicit `branch.<head>.remote` naming a remote that does not exist still
/// counts, because `remotes_remote_get_1()` installs the name as a URL alias when
/// it was given explicitly (`remote.c:816-817`) and `valid_remote()` then holds.
#[test]
fn an_explicit_branch_remote_counts_even_with_no_remote_section() {
    let (root, r) = empty_repo("explicit-branch-remote");
    ok(&r, &["config", "branch.main.remote", "nowhere"]);
    let out = git(&r, &["worktree", "add", "--guess-remote", root.join("w").to_str().unwrap()]);

    assert_eq!(code(&out), 128, "{}", err(&out));
    assert!(err(&out).contains("No local or remote refs exist despite at least one remote"));
}

/// A remote-tracking branch matching the path's basename is a source branch, so
/// `dwim_branch()` returns it and `dwim_orphan()` is never asked
/// (`worktree.c:895-898`). The new branch starts there rather than unborn.
#[test]
fn a_matching_remote_tracking_branch_prevents_the_inference() {
    let (root, r) = empty_repo("remote-ref");
    let up = root.join("up");
    std::fs::create_dir_all(&up).unwrap();
    ok(&up, &["init", "-q", "-b", "main", "."]);
    std::fs::write(up.join("f.txt"), "a\n").unwrap();
    ok(&up, &["add", "f.txt"]);
    ok(&up, &["commit", "-q", "-m", "base"]);
    ok(&up, &["branch", "topic"]);
    ok(&r, &["remote", "add", "origin", up.to_str().unwrap()]);
    ok(&r, &["fetch", "-q", "origin"]);

    let out = git(&r, &["worktree", "add", "--guess-remote", root.join("topic").to_str().unwrap()]);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert!(
        !err(&out).contains("No possible source branch"),
        "a fetched remote branch is a source branch, got {:?}",
        err(&out)
    );
    assert_eq!(wt_head(&r, "topic"), "ref: refs/heads/topic");
    // Unborn worktrees have no commit; this one does, which is how the two
    // outcomes are told apart on disk.
    let rev = git(&r, &["rev-parse", "--verify", "refs/heads/topic"]);
    assert_eq!(code(&rev), 0, "{}", err(&rev));
}

/// `--detach` on a repository with no refs does *not* infer `--orphan`: its arm
/// only calls `can_use_local_refs()` (`worktree.c:883-885`) and then falls into
/// the `invalid reference` floor, hint and all (`worktree.c:919-930`).
#[test]
fn detach_on_an_empty_repo_dies_with_the_orphan_hint() {
    let (root, r) = empty_repo("detach-empty");
    let wt = root.join("w");
    let out = git(&r, &["worktree", "add", "--detach", wt.to_str().unwrap()]);

    assert_eq!(code(&out), 128, "{}", err(&out));
    assert!(
        err(&out).contains("hint: If you meant to create a worktree containing a new unborn branch")
            && err(&out).contains("fatal: invalid reference: HEAD"),
        "got {:?}",
        err(&out)
    );
    assert!(
        !err(&out).contains("No possible source branch"),
        "--detach never reaches dwim_orphan(), got {:?}",
        err(&out)
    );
    assert!(!wt.exists());
}

/// `can_use_local_refs()` warns when branches exist but `HEAD` does not resolve
/// (`worktree.c:695-697`). The `ac == 2` arm asks the same question whenever the
/// `<commit-ish>` spelled out is `HEAD` (`worktree.c:910-911`) — and that arm
/// gets no orphan hint, because the user named a start point rather than omitting
/// one.
#[test]
fn an_explicit_head_commitish_warns_about_the_broken_head_without_the_hint() {
    let (root, r) = repo_with_commit("explicit-head");
    ok(&r, &["branch", "other"]);
    std::fs::write(r.join(".git/HEAD"), "ref: refs/heads/nope\n").unwrap();

    let out = git(&r, &["worktree", "add", root.join("w").to_str().unwrap(), "HEAD"]);
    assert_eq!(code(&out), 128, "{}", err(&out));
    assert!(
        err(&out).contains("warning: HEAD points to an invalid (or orphaned) reference."),
        "got {:?}",
        err(&out)
    );
    assert!(
        !err(&out).contains("hint: If you meant to create a worktree"),
        "the hint is for the ac < 2 forms only, got {:?}",
        err(&out)
    );
    assert!(err(&out).contains("fatal: invalid reference: HEAD"));
}

/// The same broken `HEAD` reached through the DWIM arm warns exactly once — the
/// warning belongs to `can_use_local_refs()`, which each arm calls a single time.
#[test]
fn the_broken_head_warning_is_printed_once_not_twice() {
    let (root, r) = repo_with_commit("warn-once");
    ok(&r, &["branch", "other"]);
    std::fs::write(r.join(".git/HEAD"), "ref: refs/heads/nope\n").unwrap();

    let out = git(&r, &["worktree", "add", root.join("w").to_str().unwrap()]);
    assert_eq!(code(&out), 128, "{}", err(&out));
    assert_eq!(
        err(&out).matches("HEAD points to an invalid (or orphaned) reference.").count(),
        1,
        "got {:?}",
        err(&out)
    );
    assert!(err(&out).contains("hint: If you meant to create a worktree"));
}

/// A repository that *can* start a worktree never mentions the inference. The
/// control for every test above: if this ever printed the line, they would all
/// pass vacuously.
#[test]
fn a_repo_with_a_commit_never_infers_orphan() {
    let (root, r) = repo_with_commit("no-infer");
    let out = git(&r, &["worktree", "add", root.join("w").to_str().unwrap()]);

    assert_eq!(code(&out), 0, "{}", err(&out));
    assert!(!err(&out).contains("No possible source branch"), "got {:?}", err(&out));
    assert!(out_str(&out).contains("HEAD is now at"), "got {:?}", out_str(&out));
}

/// `worktree.c:848-849`: `--reason` is only the text `--lock` writes into the
/// administrative `locked` file, so on its own it is a `die()`, not a no-op.
#[test]
fn reason_without_lock_is_fatal_and_creates_nothing() {
    let (root, r) = repo_with_commit("reason-only");
    let wt = root.join("w");
    let out = git(&r, &["worktree", "add", "--reason", "why not", wt.to_str().unwrap()]);

    assert_eq!(code(&out), 128, "{}", err(&out));
    assert_eq!(err(&out), "fatal: the option '--reason' requires '--lock'\n");
    assert!(!wt.exists(), "the refusal must precede any work");
    assert!(!r.join(".git/worktrees/w").exists());
}

/// The check sits after the `--orphan` pairings (`worktree.c:839-847`) and before
/// the argument count (`:855`), so an `--orphan --reason` with no `--lock` still
/// reports `--reason`, and a `--reason` with no path at all reports it too rather
/// than the usage block.
#[test]
fn the_reason_check_runs_after_orphan_pairs_and_before_the_argument_count() {
    let (root, r) = repo_with_commit("reason-order");

    let out = git(&r, &["worktree", "add", "--orphan", "--reason", "r", root.join("a").to_str().unwrap()]);
    assert_eq!(code(&out), 128, "{}", err(&out));
    assert_eq!(err(&out), "fatal: the option '--reason' requires '--lock'\n");

    let out = git(&r, &["worktree", "add", "--reason", "r"]);
    assert_eq!(code(&out), 128, "{}", err(&out));
    assert_eq!(err(&out), "fatal: the option '--reason' requires '--lock'\n");

    // `--orphan --no-checkout` is checked earlier still, so it wins over the
    // `--reason` refusal that would otherwise apply to the same command line.
    let out = git(
        &r,
        &["worktree", "add", "--orphan", "--no-checkout", "--reason", "r", root.join("b").to_str().unwrap()],
    );
    assert_eq!(code(&out), 128, "{}", err(&out));
    assert_eq!(err(&out), "fatal: options '--orphan' and '--no-checkout' cannot be used together\n");
}

/// With `--lock` the reason is accepted and lands verbatim in `locked`
/// (`worktree.c:529-530`); bare `--lock` writes git's own default text
/// (`worktree.c:852-853`).
#[test]
fn lock_writes_the_reason_or_the_default_text() {
    let (root, r) = repo_with_commit("lock-text");

    let a = root.join("a");
    ok(&r, &["worktree", "add", "--lock", "--reason", "why not", a.to_str().unwrap()]);
    // `write_file()` terminates the line, which `t2400`'s `echo … >expect`
    // comparison also depends on.
    assert_eq!(std::fs::read_to_string(r.join(".git/worktrees/a/locked")).unwrap(), "why not\n");

    let b = root.join("b");
    ok(&r, &["worktree", "add", "--lock", b.to_str().unwrap()]);
    assert_eq!(
        std::fs::read_to_string(r.join(".git/worktrees/b/locked")).unwrap(),
        "added with --lock\n"
    );
}
