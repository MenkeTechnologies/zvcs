//! `git show --graph` without a range.
//!
//! `cmd_show()` starts with `rev.no_walk = 1`, and `setup_revisions()` refuses
//! `--graph` beside it after every argument is read (revision.c:3235-3236), so
//! `git show --graph <commit>` dies with "options '--no-walk' and '--graph'
//! cannot be used together" and exit 128. Ahead of that it runs
//! `die_for_incompatible_opt3()` over `--graph`, `--reverse` and
//! `--walk-reflogs` (revision.c:3228-3230), and `--graph` sets `topo_order`
//! (revision.c:2788), which limits the walk and so trips "cannot combine
//! --walk-reflogs with history-limiting options" first (revision.c:3195-3196,
//! :3218-3219). zvcs refused the option outright with exit 1. `--no-graph`
//! cancels it.
//!
//! Every expectation is stock git's output for the same command on the same
//! fixture.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, root: &Path, args: &[&str]) -> Output {
    Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root)
        .env("ZVCS_HOME", root.join(".zvcs"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .unwrap()
}

struct Fixture {
    root: PathBuf,
    work: PathBuf,
    stock: &'static str,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Option<Self> {
        let stock = stock_git::stock_git()?;
        let root = std::env::temp_dir().join(format!("zvcs-show-graph-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let fx = Fixture { root, work, stock };
        fx.stock(&["init", "-q", "-b", "main", "."]);
        fx.stock(&["commit", "-q", "--allow-empty", "-m", "one"]);
        fx.stock(&["commit", "-q", "--allow-empty", "-m", "two"]);
        Some(fx)
    }

    fn stock(&self, args: &[&str]) {
        let out = run(self.stock, &self.work, &self.root, args);
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn same(&self, args: &[&str]) {
        let want = run(self.stock, &self.work, &self.root, args);
        let got = run(BIN, &self.work, &self.root, args);
        assert_eq!(
            (String::from_utf8_lossy(&got.stdout), String::from_utf8_lossy(&got.stderr), got.status.code()),
            (String::from_utf8_lossy(&want.stdout), String::from_utf8_lossy(&want.stderr), want.status.code()),
            "{args:?}"
        );
    }
}

#[test]
fn graph_without_a_range_meets_no_walk() {
    let Some(fx) = Fixture::new("nowalk") else { return };
    fx.same(&["show", "--graph", "HEAD"]);
    fx.same(&["show", "--graph", "-s", "HEAD~1", "HEAD"]);
    fx.same(&["show", "--graph", "--oneline"]);
    fx.same(&["show", "--graph", "--reverse", "HEAD"]);
    fx.same(&["show", "--graph", "-g", "HEAD"]);
    fx.same(&["show", "--graph", "--reverse", "-g", "HEAD"]);
    fx.same(&["show", "--graph", "--no-graph", "-s", "HEAD"]);
    fx.same(&["show", "--no-graph", "--graph", "--stat", "HEAD"]);
}
