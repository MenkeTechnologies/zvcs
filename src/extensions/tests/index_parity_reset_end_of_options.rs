//! `git reset --end-of-options`: the marker that lets a revision be spelled
//! like an option.
//!
//! ```c
//! } else if (!strcmp(arg + 2, "end-of-options")) {
//!         if (!(ctx->flags & PARSE_OPT_KEEP_UNKNOWN_OPT)) {
//!                 ctx->argc--;
//!                 ctx->argv++;
//!         }
//!         break;
//! }
//! ```
//!
//! (parse-options.c:1116-1122.) `cmd_reset()` calls `parse_options()` with
//! `PARSE_OPT_KEEP_DASHDASH` and no `PARSE_OPT_KEEP_UNKNOWN_OPT`
//! (builtin/reset.c:386-387), so the marker is consumed and everything after it
//! is `parse_args()`'s (builtin/reset.c:180-215) — a revision, then `--`, then
//! paths. The port had no such state and refused `--foo` with `error: unknown
//! option \`foo'` and exit 129, which is t7102-reset.sh's case 38.
//!
//! The marker is tested with a `strcmp()` ahead of `parse_long_opt()`, so it
//! never abbreviates and is never confused with an option in the table.
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
    /// Two commits on `main`, plus a branch whose name begins with `--`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-idx-eoo-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "one\n").unwrap();
        f.git(&["add", "a"]);
        f.git(&["commit", "-q", "-m", "first"]);
        std::fs::write(f.work.join("a"), "two\n").unwrap();
        f.git(&["commit", "-q", "-a", "-m", "second"]);
        // `update-ref` writes the name `branch` would refuse to parse as an option.
        f.git(&["update-ref", "refs/heads/--foo", "HEAD^"]);
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

    fn subject(&self, rev: &str) -> String {
        let out = self.cmd(&["log", "-1", "--format=%s", rev]).output().unwrap();
        assert!(out.status.success(), "log failed: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }
}

/// t7102-reset.sh's case 38: the branch `--foo` is reachable as a revision.
#[test]
fn index_parity_reset_end_of_options_makes_the_next_token_a_revision() {
    let f = Fixture::new("rev");
    assert_eq!(f.subject("HEAD"), "second");

    let (_, _, code) = f.run(&["reset", "--hard", "--end-of-options", "--foo"]);
    assert_eq!(code, 0, "reset --end-of-options --foo was refused");
    assert_eq!(f.subject("HEAD"), "first");
    assert_eq!(std::fs::read_to_string(f.work.join("a")).unwrap(), "one\n");
}

/// Without the marker the same token is an unknown option: `error: unknown
/// option \`foo'` at 129, the usage-error status `parse_options()` exits with.
#[test]
fn index_parity_reset_without_end_of_options_still_refuses_a_dash_dash_token() {
    let f = Fixture::new("norev");
    let (out, err, code) = f.run(&["reset", "--hard", "--foo"]);
    assert_eq!(out, "", "a refused reset wrote to stdout");
    assert_eq!(code, 129, "expected the usage-error status");
    assert!(
        err.starts_with("error: unknown option `foo'"),
        "unexpected refusal: {err}"
    );
    assert_eq!(f.subject("HEAD"), "second", "the refused reset moved HEAD");
}

/// After the marker `--` is still `parse_args()`'s separator, so a revision and
/// a pathspec can both follow it.
#[test]
fn index_parity_reset_end_of_options_keeps_the_pathspec_separator() {
    let f = Fixture::new("paths");
    std::fs::write(f.work.join("a"), "dirty\n").unwrap();
    f.git(&["add", "a"]);

    // `<rev> -- <path>` after the marker: a mixed reset of just that path. The
    // refresh that follows names `a`, whose worktree copy stays dirty.
    let (out, err, code) = f.run(&["reset", "--end-of-options", "--foo", "--", "a"]);
    assert_eq!(
        (out.as_str(), code),
        ("Unstaged changes after reset:\nM\ta\n", 0),
        "reset failed: {err}"
    );
    // The index now holds `--foo`'s version of `a`, while the worktree keeps the
    // dirty content a mixed reset never touches.
    assert_eq!(
        f.run(&["show", ":a"]).0,
        "one\n",
        "the path was not reset to the named revision"
    );
    assert_eq!(std::fs::read_to_string(f.work.join("a")).unwrap(), "dirty\n");
    assert_eq!(f.subject("HEAD"), "second", "a path reset moved HEAD");
}
