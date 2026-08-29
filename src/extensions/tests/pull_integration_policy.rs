//! What `git pull` refuses to decide for you, and what it integrates when it
//! does decide.
//!
//! * A diverged branch with no policy — no `--ff…`, no `pull.ff`, no
//!   `pull.rebase`/`branch.<name>.rebase` — is not quietly merged: git prints
//!   the `advice.diverging` block and dies (`cmd_pull()`, builtin/pull.c). A
//!   merge commit made where git makes none is history the user never asked for.
//! * An unfinished merge stops the pull *before* the fetch, with
//!   `die_resolve_conflict()`/`die_conclude_merge()`.
//! * `run_merge()` hands the merge a literal `FETCH_HEAD`, so several refspecs
//!   become an octopus and the commit is named from the fetch's own
//!   descriptions (`Merge branch 'main' of <url>`), not from a tracking ref.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// An upstream with `main` and `side` both ahead of what the clone has.
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
        let root = std::env::temp_dir().join(format!("zvcs-pullpol-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let srv = root.join("srv");
        let work = root.join("work");
        std::fs::create_dir_all(&srv).unwrap();
        let f = Fixture { root, srv, work };

        std::fs::write(f.srv.join("a.txt"), "one\n").unwrap();
        std::fs::write(f.srv.join("o.txt"), "o\n").unwrap();
        f.run_in(&f.srv, &["init", "-q", "-b", "main", "."]);
        f.run_in(&f.srv, &["config", "user.email", "t@e.co"]);
        f.run_in(&f.srv, &["config", "user.name", "t"]);
        f.run_in(&f.srv, &["add", "-A"]);
        f.run_in(&f.srv, &["commit", "-q", "-m", "one"]);

        f.run_in(&f.root, &["clone", "-q", f.srv.to_str().unwrap(), "work"]);
        f.run_in(&f.work, &["config", "user.email", "t@e.co"]);
        f.run_in(&f.work, &["config", "user.name", "t"]);

        // `side` forks from what the clone already has, so neither it nor `main`
        // is an ancestor of the other — the shape that actually needs an octopus.
        f.run_in(&f.srv, &["checkout", "-q", "-b", "side"]);
        std::fs::write(f.srv.join("side.txt"), "s\n").unwrap();
        f.run_in(&f.srv, &["add", "side.txt"]);
        f.run_in(&f.srv, &["commit", "-q", "-m", "sidework"]);
        f.run_in(&f.srv, &["checkout", "-q", "main"]);
        std::fs::write(f.srv.join("a.txt"), "one\ntwo\n").unwrap();
        f.run_in(&f.srv, &["commit", "-q", "-am", "upstream"]);
        f
    }

    /// The upstream path as it is recorded in FETCH_HEAD: the string `clone`
    /// was handed, VERBATIM. git stores the url it was given — it does not
    /// resolve symlinks — so on macOS, where `std::env::temp_dir()` returns a
    /// `/var/folders/…` path that is a symlink to `/private/var/folders/…`,
    /// `remote.origin.url`, FETCH_HEAD and the merge title all carry the
    /// `/var/…` spelling. Canonicalising here asserted `/private/var/…` and
    /// failed on every macOS runner while passing on Linux, where the two
    /// spellings coincide.
    fn srv_url(&self) -> String {
        self.srv.display().to_string()
    }

    /// A local commit the upstream does not have, which makes the branch diverge.
    fn diverge(&self) {
        std::fs::write(self.work.join("o.txt"), "local\n").unwrap();
        self.run_in(&self.work, &["commit", "-q", "-am", "localwork"]);
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

    fn log_subjects(&self) -> Vec<String> {
        let out = self.cmd_in(&self.work, &["log", "--format=%s"]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).lines().map(str::to_owned).collect()
    }
}

/// No configured policy plus a diverged branch: the advice block, exit 128, and
/// — the part that matters — no merge commit.
#[test]
fn diverged_branches_without_a_policy_are_refused() {
    let f = Fixture::new("diverging");
    f.diverge();

    let (code, out, err) = f.run(&["pull"]);
    assert_eq!(code, 128, "wrong exit: {out}{err}");
    assert!(
        err.contains("hint: You have divergent branches and need to specify how to reconcile them.")
            && err.contains("hint:   git config pull.rebase false  # merge")
            && err.contains("fatal: Need to specify how to reconcile divergent branches."),
        "stderr: {err}"
    );
    assert_eq!(
        f.log_subjects(),
        ["localwork", "one"],
        "nothing may be integrated when git refuses to choose"
    );
}

/// The same pull with a policy configured goes through, and the merge commit is
/// named from FETCH_HEAD — `Merge branch 'main' of <url>`, not after a
/// remote-tracking ref.
#[test]
fn a_configured_policy_merges_and_names_the_commit_from_fetch_head() {
    let f = Fixture::new("policy");
    f.diverge();

    let (code, out, err) = f.run(&["-c", "pull.rebase=false", "pull"]);
    assert_eq!(code, 0, "pull failed: {out}{err}");

    let subject = f.log_subjects().first().cloned().unwrap_or_default();
    assert_eq!(
        subject,
        format!("Merge branch 'main' of {}", f.srv_url()),
        "merge commit is not named the way the fetch described the head"
    );
    // `advice.diverging=false` silences the hint but not the refusal, so the
    // policy — not the advice setting — is what let this through.
    assert!(!err.contains("Need to specify"), "stderr: {err}");
}

/// Two refspecs are two merge heads: an octopus, named by the grouped
/// descriptions git's `fmt_merge_msg_title()` produces.
#[test]
fn several_refspecs_become_an_octopus() {
    let f = Fixture::new("octopus");

    let (code, out, err) = f.run(&["pull", "--no-rebase", "origin", "main", "side"]);
    assert_eq!(code, 0, "octopus pull failed: {out}{err}");
    assert!(
        f.work.join("side.txt").exists(),
        "the second head was not integrated: {out}{err}"
    );

    let subject = f.log_subjects().first().cloned().unwrap_or_default();
    assert_eq!(
        subject,
        format!("Merge branches 'main' and 'side' of {}", f.srv_url()),
        "octopus title does not group the heads by source"
    );

    // A rebase cannot take several heads, and neither can a fast-forward.
    let f = Fixture::new("octopus-refusals");
    let (code, _, err) = f.run(&["pull", "--rebase", "origin", "main", "side"]);
    assert_eq!(code, 128, "stderr: {err}");
    assert!(err.contains("fatal: Cannot rebase onto multiple branches."), "stderr: {err}");
    let (code, _, err) = f.run(&["pull", "--ff-only", "origin", "main", "side"]);
    assert_eq!(code, 128, "stderr: {err}");
    assert!(
        err.contains("fatal: Cannot fast-forward to multiple branches."),
        "stderr: {err}"
    );
}

/// An unfinished merge ends the pull before the fetch runs: no `From …` summary,
/// and the tracking ref has not moved.
#[test]
fn an_unfinished_merge_stops_the_pull_before_the_fetch() {
    let f = Fixture::new("unfinished");
    // A conflicted merge, left in place.
    f.run_in(&f.work, &["checkout", "-q", "-b", "other"]);
    std::fs::write(f.work.join("o.txt"), "other\n").unwrap();
    f.run_in(&f.work, &["commit", "-q", "-am", "other"]);
    f.run_in(&f.work, &["checkout", "-q", "main"]);
    std::fs::write(f.work.join("o.txt"), "mine\n").unwrap();
    f.run_in(&f.work, &["commit", "-q", "-am", "mine"]);
    let _ = f.run(&["merge", "other"]); // expected to conflict

    let before = f.run(&["rev-parse", "refs/remotes/origin/main"]).1;
    let (code, out, err) = f.run(&["pull"]);
    assert_eq!(code, 128, "wrong exit: {out}{err}");
    assert!(
        err.starts_with("error: Pulling is not possible because you have unmerged files.")
            && err.contains("fatal: Exiting because of an unresolved conflict."),
        "stderr: {err}"
    );
    assert!(!err.contains("From "), "the fetch must not have run: {err}");
    assert_eq!(before, f.run(&["rev-parse", "refs/remotes/origin/main"]).1);
}
