//! `git branch`'s branch-name *operands* go through `copy_branchname()`
//! (`refs.c:747-760`), which runs `repo_interpret_branch_name()` over them
//! before they are spliced under `refs/heads/`.
//!
//! What that buys, and what it deliberately does not:
//!
//!   * `@{-<n>}` names the branch left `n` checkouts ago, read off `HEAD`'s
//!     reflog by `interpret_nth_prior_checkout()` (`object-name.c:1273-1306`).
//!   * `<branch>@{upstream}` / `@{u}` names that branch's upstream, shortened
//!     by `set_shortened_ref()`.
//!   * `branch_interpret_allowed()` (`object-name.c:1412-1426`) drops a rewrite
//!     that lands outside the namespace the caller asked for, so `-r -d @{-1}`
//!     will not delete a local branch and `-d @{upstream}` will not delete a
//!     remote-tracking one — the operand is left as typed and simply not found.
//!   * A `--list` pattern is not an operand: `git branch --list '@{-1}'` still
//!     matches by name.
//!   * `interpret_empty_at()` is gated on `INTERPRET_BRANCH_HEAD`, which
//!     `builtin/branch.c` never passes, so a branch literally named `@` is
//!     reachable as `@`.
//!
//! Every expectation was measured from stock git 2.55.0 in an identical
//! throwaway repository under the same pinned environment.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// Two commits on `main`, so a branch can be moved to a distinguishable tip.
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("zvcs-br-branchname-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "one\n").unwrap();
        f.git(&["add", "a"]);
        f.git(&["commit", "-q", "-m", "one"]);
        f.git(&["tag", "one"]);
        std::fs::write(f.work.join("a"), "two\n").unwrap();
        f.git(&["commit", "-q", "-am", "two"]);
        f.git(&["tag", "two"]);
        // `validate_remote_tracking_branch()` (branch.c:501-504) only accepts a
        // `refs/remotes/` ref that some remote's fetch refspec maps onto, so the
        // remote-tracking half of these tests needs a remote to exist.
        f.git(&["remote", "add", "origin", "foo.git"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = self.cmd(args).output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn stdout(&self, args: &[&str]) -> String {
        let (out, err, code) = self.run(args);
        assert_eq!((err.as_str(), code), ("", 0), "`git {args:?}`");
        out
    }

    fn oid(&self, spec: &str) -> String {
        self.stdout(&["rev-parse", spec]).trim().to_string()
    }

    fn has_ref(&self, name: &str) -> bool {
        self.run(&["rev-parse", "--verify", "--quiet", name]).2 == 0
    }
}

/// `git branch -f @{-1} <start>` moves the branch left one checkout ago, and
/// `git branch -D @{-1}` deletes it: the creation path reaches
/// `copy_branchname()` through `validate_branchname()` (`branch.c:375`), the
/// delete path directly (`builtin/branch.c:262`).
#[test]
fn nth_prior_checkout_names_the_branch_for_create_and_delete() {
    let f = Fixture::new("nth");
    f.git(&["branch", "previous", "one"]);
    f.git(&["checkout", "-q", "previous"]);
    f.git(&["checkout", "-q", "main"]);

    f.git(&["branch", "-f", "@{-1}", "two"]);
    assert_eq!(f.oid("refs/heads/previous"), f.oid("two"));

    f.git(&["branch", "-D", "@{-1}"]);
    assert!(!f.has_ref("refs/heads/previous"));
}

/// The upstream mark resolves against `branch.<name>.merge`, and an empty
/// left-hand side is the current branch (`branch_get(NULL)`).
#[test]
fn local_upstream_mark_names_the_upstream_branch() {
    let f = Fixture::new("upstream");
    f.git(&["branch", "local", "one"]);
    f.git(&["branch", "--set-upstream-to=local"]);

    f.git(&["branch", "-f", "@{upstream}", "two"]);
    assert_eq!(f.oid("refs/heads/local"), f.oid("two"));

    f.git(&["branch", "-D", "@{u}"]);
    assert!(!f.has_ref("refs/heads/local"));
}

/// `branch_interpret_allowed()` refuses a rewrite outside the requested
/// namespace, in both directions: `-r -D @{-1}` (a local branch under
/// `INTERPRET_BRANCH_REMOTE`) and `-D @{upstream}` when the upstream is a
/// remote-tracking ref. Both leave the operand as typed, so the refusal is the
/// ordinary "not found", and neither branch is touched.
#[test]
fn a_rewrite_outside_the_allowed_namespace_is_dropped() {
    let f = Fixture::new("allowed");
    f.git(&["update-ref", "refs/remotes/origin/prev", "one"]);
    f.git(&["checkout", "-q", "-b", "origin/prev", "two"]);
    f.git(&["checkout", "-q", "main"]);

    let (out, err, code) = f.run(&["branch", "-r", "-D", "@{-1}"]);
    assert_eq!((out.as_str(), code), ("", 1));
    assert!(err.contains("remote-tracking branch '@{-1}' not found"), "{err}");
    assert_eq!(f.oid("refs/remotes/origin/prev"), f.oid("one"));
    assert_eq!(f.oid("refs/heads/origin/prev"), f.oid("two"));

    f.git(&["update-ref", "refs/remotes/origin/rdel", "two"]);
    f.git(&["branch", "--set-upstream-to=origin/rdel"]);
    let (out, err, code) = f.run(&["branch", "-D", "@{upstream}"]);
    assert_eq!((out.as_str(), code), ("", 1));
    assert!(err.contains("branch '@{upstream}' not found"), "{err}");
    assert!(f.has_ref("refs/remotes/origin/rdel"));
}

/// `-r -D @{upstream}` is the allowed direction, and deletes the
/// remote-tracking ref the current branch tracks.
#[test]
fn remote_upstream_mark_is_deletable_under_dash_r() {
    let f = Fixture::new("remote-up");
    f.git(&["update-ref", "refs/remotes/origin/rdel", "two"]);
    f.git(&["branch", "--set-upstream-to=origin/rdel"]);

    f.git(&["branch", "-r", "-D", "@{upstream}"]);
    assert!(!f.has_ref("refs/remotes/origin/rdel"));
}

/// A `--list` pattern is not an operand, and a branch literally named `@` stays
/// itself because `builtin/branch.c` never passes `INTERPRET_BRANCH_HEAD`.
#[test]
fn list_patterns_and_a_branch_named_at_are_not_rewritten() {
    let f = Fixture::new("literal");
    f.git(&["branch", "previous", "one"]);
    f.git(&["checkout", "-q", "previous"]);
    f.git(&["checkout", "-q", "main"]);

    assert_eq!(f.stdout(&["branch", "--list", "@{-1}"]), "");

    f.git(&["branch", "-f", "@", "one"]);
    assert_eq!(f.oid("refs/heads/@"), f.oid("one"));
    f.git(&["branch", "-D", "@"]);
    assert!(!f.has_ref("refs/heads/@"));
}

/// `--edit-description`, `--set-upstream-to` and `--unset-upstream` read their
/// operand through `copy_branchname()` too (`builtin/branch.c:901`, `:944`,
/// `:974`), so `@{-1}` selects the previously checked-out branch there as well.
#[test]
fn upstream_options_interpret_their_operand() {
    let f = Fixture::new("upstream-opts");
    f.git(&["branch", "other", "one"]);
    f.git(&["checkout", "-q", "-b", "prev", "one"]);
    f.git(&["checkout", "-q", "main"]);

    f.git(&["branch", "--set-upstream-to", "other", "@{-1}"]);
    assert_eq!(f.stdout(&["config", "branch.prev.merge"]).trim(), "refs/heads/other");

    f.git(&["branch", "--unset-upstream", "@{-1}"]);
    assert_eq!(f.run(&["config", "branch.prev.merge"]).2, 1);
}
