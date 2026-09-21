//! Two things `git checkout` does to its operands before it uses them, and did
//! not do here: resolve a start-point through `get_oid_mb()`, and run a `-b`/
//! `-B` name through `check_branch_ref()`.
//!
//! **Start-point.** `parse_branchname_arg()` (builtin/checkout.c:1476) and
//! `dwim_branch_start()` (branch.c:545) both resolve with `repo_get_oid_mb()`
//! (object-name.c:1308-1353), where a name holding `...` is the *merge base* of
//! its two sides and an empty side means `HEAD`. The port used plain
//! `repo_get_oid()`, so `git checkout branch1...` reported `pathspec
//! 'branch1...' did not match any file(s)` and `git checkout -b b2 branch1...`
//! died with `'branch1...' is not a commit`. `git branch`'s start-point already
//! had the port of that function; all of them now share it.
//!
//! **New-branch name.** `cmd_checkout()` puts `opts->new_branch` through
//! `validate_branchname()` / `validate_new_branchname()`
//! (builtin/checkout.c:2065-2074), and both begin with `check_branch_ref()`,
//! whose first act is `copy_branchname()` (refs.c:762-765) — an
//! `interpret_branch_name()` pass that rewrites `@{-<n>}` and `<branch>@{u}`.
//! The port validated the operand verbatim, so `git checkout -b @{-1}` was
//! refused as an invalid branch name where git names the branch the mark
//! resolves to.
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
    /// `main` at two commits, `side` left behind at the first, so their merge
    /// base is that first commit and is not the tip of either.
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("zvcs-idx-ckname-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "one\n").unwrap();
        f.git(&["add", "a"]);
        f.git(&["commit", "-q", "-m", "one"]);
        f.git(&["branch", "side"]);
        std::fs::write(f.work.join("a"), "two\n").unwrap();
        f.git(&["commit", "-q", "-a", "-m", "two"]);
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

    fn run(&self, args: &[&str]) -> (String, i32) {
        let out = self.cmd(args).output().unwrap();
        (
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn oid(&self, rev: &str) -> String {
        let out = self.cmd(&["rev-parse", rev]).output().unwrap();
        assert!(out.status.success(), "rev-parse {rev} failed: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }
}

/// t2018-checkout-branch.sh's cases 4 and 13: `-b`/`-B` start the branch at the
/// merge base. The empty right-hand side means `HEAD`.
#[test]
fn index_parity_checkout_b_accepts_a_merge_base_start_point() {
    let f = Fixture::new("mbstart");
    let base = f.oid("side");

    let (err, code) = f.run(&["checkout", "-b", "b2", "side..."]);
    assert_eq!(code, 0, "checkout -b <a>... was refused: {err}");
    assert_eq!(f.oid("HEAD"), base);
    assert_eq!(f.oid("b2"), base);

    f.git(&["checkout", "-q", "main"]);
    let (err, code) = f.run(&["checkout", "-B", "b2", "side...main"]);
    assert_eq!(code, 0, "checkout -B <a>...<b> was refused: {err}");
    assert_eq!(f.oid("b2"), base);
}

/// The same resolution without `-b`: a detaching checkout at the merge base,
/// which the port had been reporting as an unmatched pathspec.
#[test]
fn index_parity_checkout_detaches_at_a_merge_base() {
    let f = Fixture::new("mbdetach");
    let base = f.oid("side");

    let (err, code) = f.run(&["checkout", "side..."]);
    assert_eq!(code, 0, "checkout <a>... was refused: {err}");
    assert_eq!(f.oid("HEAD"), base);
    // It is a detach, not a branch switch.
    assert_ne!(code, 1);
    assert_eq!(f.run(&["symbolic-ref", "-q", "HEAD"]).1, 1);
}

/// t2018-checkout-branch.sh's case 11: `-b @{-1}` names the previously checked
/// out branch, so the refusal is about *that* branch already existing.
#[test]
fn index_parity_checkout_b_interprets_an_at_mark_branch_name() {
    let f = Fixture::new("atmark");
    f.git(&["branch", "other"]);
    f.git(&["checkout", "-q", "other"]);
    f.git(&["checkout", "-q", "main"]);

    let (err, code) = f.run(&["checkout", "-b", "@{-1}"]);
    assert_eq!(code, 128, "expected the die() status: {err}");
    assert_eq!(err, "fatal: a branch named 'other' already exists\n");

    // With that branch gone the mark still resolves, and the branch created is
    // the rewritten name — never a ref literally called `@{-1}`.
    f.git(&["branch", "-D", "other"]);
    let (err, code) = f.run(&["checkout", "-b", "@{-1}"]);
    assert_eq!(code, 0, "checkout -b @{{-1}} was refused: {err}");
    assert_eq!(f.oid("refs/heads/other"), f.oid("HEAD"));
    assert_eq!(
        f.run(&["rev-parse", "--verify", "--quiet", "refs/heads/@{-1}"]).1,
        1,
        "a ref was created under the literal mark"
    );
}

/// An ordinary name is untouched by the rewrite, and one `check_refname_format()`
/// rejects is still refused — naming the operand exactly as typed, since that
/// refusal comes from `validate_branchname()` rather than from the rewrite.
#[test]
fn index_parity_checkout_b_still_refuses_an_invalid_branch_name() {
    let f = Fixture::new("invalid");
    let (err, code) = f.run(&["checkout", "-b", "bad..name"]);
    assert_eq!(code, 128, "expected the die() status: {err}");
    assert!(
        err.starts_with("fatal: 'bad..name' is not a valid branch name"),
        "unexpected refusal: {err}"
    );

    let (err, code) = f.run(&["checkout", "-b", "plain"]);
    assert_eq!(code, 0, "an ordinary -b was refused: {err}");
    assert_eq!(f.oid("refs/heads/plain"), f.oid("HEAD"));
}
