//! `@{-N}` and `<ref>@{N}` operands against stock git, through `rev-parse`,
//! `log`, `log -g`, `show`, `rev-list` and `checkout`.
//!
//! `get_oid_basic()` (object-name.c:702-746) refuses an `@{-` that is not at the
//! start of the name, and leaves one at the start to `repo_dwim_ref()`, which
//! rewrites it only when `interpret_nth_prior_checkout()` reads `N` with
//! `strtol()` up to the brace and the reflog holds that many switches
//! (object-name.c:1270-1306). `@{--1}`, `@{-1 }`, `@{-0x1}` and an `N` past
//! `LONG_MAX` therefore do not resolve, while `@{- 1}` and `@{-+1}` do. The
//! count lands in `int remaining`, so `@{-4294967297}` is `@{-1}`, and the
//! entry number `nth` is an `int` too, so `@{4294967297}` is `@{1}`.
//! `add_pending_object_with_path()` (revision.c:306-316) hands the reflog walk
//! the interpreted name, so `log -g @{-1}` walks the prior branch's log, and
//! `add_reflog_for_walk()` reads the entry with `strtoul()`
//! (reflog-walk.c:172-173).

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn command(bin: &str, home: &Path, dir: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env("GIT_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("GIT_DIR");
    cmd
}

impl Fixture {
    /// `main` with one commit, `side` with a second, then back on `main`:
    /// `@{-1}` is `side`, whose log has two entries; `main`'s has one.
    fn new(stock: &str, tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-nth-prior-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::create_dir_all(root.join("repo")).unwrap();
        let f = Fixture { root };
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["-c", "maintenance.auto=false", "commit", "-q", "--allow-empty", "-m", "one"],
            &["checkout", "-q", "-b", "side"],
            &["-c", "maintenance.auto=false", "commit", "-q", "--allow-empty", "-m", "two"],
            &["checkout", "-q", "main"],
        ] {
            assert!(f.run_in(stock, "repo", args).status.success(), "{args:?}");
        }
        f
    }

    fn run_in(&self, bin: &str, dir: &str, args: &[&str]) -> std::process::Output {
        command(bin, &self.root.join("home"), &self.root.join(dir), args).output().unwrap()
    }

    fn assert_same(&self, stock: &str, args: &[&str]) {
        let run = |bin: &str| {
            let out = self.run_in(bin, "repo", args);
            (String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned(), out.status.code())
        };
        assert_eq!(run(BIN), run(stock), "{args:?}");
    }
}

#[test]
fn malformed_nth_prior_does_not_resolve() {
    let Some(stock) = stock_git::stock_git() else { return };
    let f = Fixture::new(stock, "rp");
    for spec in ["@{--1}", "@{-1 }", "@{-0x1}", "@{-99999999999999999999}", "main@{--1}", "@{-1 }^0", "@{- 1}", "@{-4294967297}"] {
        f.assert_same(stock, &["rev-parse", "--symbolic-full-name", spec]);
    }
    f.assert_same(stock, &["rev-parse", "--abbrev-ref", "@{-0x1}"]);
    f.assert_same(stock, &["rev-parse", "--verify", "@{--1}"]);
    for spec in ["^@{--1}", "@{--1}..HEAD", "@{-+1}"] {
        f.assert_same(stock, &["log", "--format=%H", spec]);
        f.assert_same(stock, &["show", "-s", "--format=%H", spec]);
        f.assert_same(stock, &["rev-list", spec]);
    }
    // `@{4294967297}` wraps to entry 1 of `main`'s one-entry log.
    f.assert_same(stock, &["rev-parse", "@{4294967297}"]);
    f.assert_same(stock, &["show", "-s", "--format=%H", "@{4294967297}"]);
}

#[test]
fn reflog_walk_reads_the_interpreted_name() {
    let Some(stock) = stock_git::stock_git() else { return };
    let f = Fixture::new(stock, "walk");
    for spec in ["@{-1}", "@{- 1}", "@{-1}@{1}", "side@{4294967296}", "side@{ 1}", "@{-1}..main", "@{-1 }"] {
        f.assert_same(stock, &["log", "-g", "--format=%H %gd %gs", spec]);
    }
    f.assert_same(stock, &["reflog", "show", "@{-1}"]);
}

#[test]
fn checkout_refuses_malformed_nth_prior() {
    let Some(stock) = stock_git::stock_git() else { return };
    let f = Fixture::new(stock, "co");
    for spec in ["@{--1}", "@{-4294967297}"] {
        let mut seen = Vec::new();
        for (bin, dir) in [(stock, "a"), (BIN, "b")] {
            let copy = f.root.join(dir);
            let _ = std::fs::remove_dir_all(&copy);
            assert!(Command::new("cp").arg("-R").arg(f.root.join("repo")).arg(&copy).status().unwrap().success());
            let out = f.run_in(bin, dir, &["checkout", "-q", spec]);
            let head = f.run_in(stock, dir, &["rev-parse", "--symbolic-full-name", "HEAD"]);
            seen.push((out.stdout, out.stderr, out.status.code(), head.stdout));
        }
        assert_eq!(seen[1], seen[0], "checkout {spec:?}");
    }
}
