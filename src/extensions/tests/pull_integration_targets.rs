//! What `git pull` integrates, and when it refuses before touching the network.
//!
//! Three shapes that are easy to get wrong, all taken from `builtin/pull.c`:
//!
//! * `cmd_pull()` collects its merge heads from `FETCH_HEAD`
//!   (`get_merge_heads()`), not from a remote-tracking ref. A `<remote> <ref>`
//!   pair that lands nowhere under `refs/remotes/` — a tag, a `refs/pull/…`
//!   head — therefore integrates fine.
//! * Every fetch option pull forwards is `OPT_PASSTHRU`/`OPT_BOOL`, so the
//!   `--no-` spelling exists and reaches the fetch.
//! * `require_clean_work_tree()` runs *above* `run_fetch()` when rebasing, so a
//!   dirty tree ends the pull with exit 128 and no network traffic at all.
//! * An unborn `HEAD` is `pull_into_void()`: the fetched head becomes the
//!   initial state, with an `initial pull` reflog entry.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// An upstream repository plus a clone of it, with the upstream one commit ahead.
struct Fixture {
    root: PathBuf,
    srv: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-pulltgt-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let srv = root.join("srv");
        let work = root.join("work");
        std::fs::create_dir_all(&srv).unwrap();
        let f = Fixture { root, srv, work };

        std::fs::write(f.srv.join("a.txt"), "one\n").unwrap();
        f.run_in(&f.srv, &["init", "-q", "-b", "main", "."]);
        f.run_in(&f.srv, &["config", "user.email", "t@e.co"]);
        f.run_in(&f.srv, &["config", "user.name", "t"]);
        f.run_in(&f.srv, &["add", "-A"]);
        f.run_in(&f.srv, &["commit", "-q", "-m", "one"]);
        f.run_in(&f.srv, &["tag", "v1.0"]);

        f.run_in(&f.root, &["clone", "-q", f.srv.to_str().unwrap(), work_name()]);
        f.run_in(&f.work, &["config", "user.email", "t@e.co"]);
        f.run_in(&f.work, &["config", "user.name", "t"]);

        // The upstream moves on, so a pull has something to integrate.
        std::fs::write(f.srv.join("a.txt"), "one\ntwo\n").unwrap();
        f.run_in(&f.srv, &["commit", "-q", "-am", "two"]);
        f
    }

    fn cmd_in(&self, dir: &PathBuf, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        c
    }

    fn run_in(&self, dir: &PathBuf, args: &[&str]) {
        let out = self.cmd_in(dir, args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    /// `(exit code, stdout, stderr)` of a command run in the clone.
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = self.cmd_in(&self.work, args).output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

fn work_name() -> &'static str {
    "work"
}

/// A tag has no remote-tracking ref, so the integration has to come from
/// `FETCH_HEAD` — the clone is already at `v1.0`, which makes this a no-op pull
/// rather than a "couldn't find remote ref" failure.
#[test]
fn pulling_a_tag_integrates_from_fetch_head() {
    let f = Fixture::new("tag");
    let (code, out, err) = f.run(&["pull", "origin", "v1.0"]);
    assert_eq!(code, 0, "pulling a tag failed: {out}{err}");
    assert!(out.contains("Already up to date."), "stdout: {out}{err}");
}

/// The negated fetch options pull's own usage advertises have to be accepted and
/// forwarded, not rejected as unknown flags.
#[test]
fn negated_fetch_options_are_accepted() {
    let f = Fixture::new("negations");
    for flag in ["--no-tags", "--no-prune", "--no-force", "--no-all"] {
        let (code, out, err) = f.run(&["pull", flag]);
        assert_eq!(code, 0, "`pull {flag}` failed: {out}{err}");
    }
    // The pull did happen — the first of those four fast-forwarded the branch.
    let (_, log, _) = f.run(&["log", "--oneline"]);
    assert_eq!(log.lines().count(), 2, "the pull did not integrate: {log}");
}

/// A rebase over a dirty tree is refused *before* the fetch: git prints its two
/// error lines, exits 128, and no `From …` summary is ever produced.
#[test]
fn rebase_over_a_dirty_tree_refuses_before_fetching() {
    let f = Fixture::new("dirty-rebase");
    std::fs::write(f.work.join("a.txt"), "dirty\n").unwrap();

    let (code, out, err) = f.run(&["pull", "--rebase"]);
    assert_eq!(code, 128, "wrong exit: {out}{err}");
    assert!(
        err.contains("error: cannot pull with rebase: You have unstaged changes.")
            && err.contains("error: Please commit or stash them."),
        "stderr: {err}"
    );
    assert!(!err.contains("From "), "the fetch must not have run: {err}");

    // The tracking ref is untouched, which is the observable half of "no fetch".
    let (_, remote_tip, _) = f.run(&["rev-parse", "refs/remotes/origin/main"]);
    let (_, local_tip, _) = f.run(&["rev-parse", "HEAD"]);
    assert_eq!(remote_tip, local_tip, "origin/main moved despite the refusal");
}

/// `git init && git remote add && git pull <remote> <branch>` — the first pull
/// into a repository that has no commit yet.
#[test]
fn pull_into_an_unborn_branch_checks_out_the_fetched_head() {
    let f = Fixture::new("unborn");
    let fresh = f.root.join("fresh");
    std::fs::create_dir_all(&fresh).unwrap();
    f.run_in(&fresh, &["init", "-q", "-b", "main", "."]);
    f.run_in(&fresh, &["config", "user.email", "t@e.co"]);
    f.run_in(&fresh, &["config", "user.name", "t"]);
    f.run_in(&fresh, &["remote", "add", "origin", f.srv.to_str().unwrap()]);

    let out = f.cmd_in(&fresh, &["pull", "origin", "main"]).output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "pull into unborn failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert_eq!(std::fs::read_to_string(fresh.join("a.txt")).unwrap(), "one\ntwo\n");
    let log = f.cmd_in(&fresh, &["log", "--oneline"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&log.stdout).lines().count(), 2);
    // git records the move as `initial pull`, not as a merge or a checkout.
    let reflog = f.cmd_in(&fresh, &["reflog", "show", "HEAD"]).output().unwrap();
    assert!(
        String::from_utf8_lossy(&reflog.stdout).contains("initial pull"),
        "reflog: {}",
        String::from_utf8_lossy(&reflog.stdout)
    );
}
