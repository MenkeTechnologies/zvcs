//! A hunk header's section heading keeps a trailing form feed or vertical tab.
//! `def_ff()` (xdiff/xemit.c:102-116) and `ff_regexp()` (xdiff-interface.c:240-246)
//! strip trailing bytes with git's own `isspace()`, `sane_istest(x, GIT_SPACE)`
//! (sane-ctype.h:40), which the `sane_ctype` table grants only to tab, newline,
//! carriage return and space; `is_empty_rec()` (xdiff/xemit.c:159-167) tests the
//! same set for `-W`. zvcs trimmed with C-locale or Rust whitespace, which also
//! drops `\f` (and in some copies `\v`), so `@@ ... @@ ab^Lc^L` came out
//! `@@ ... @@ ab^Lc` across log, show, diff, diff-tree and format-patch.
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
    /// `f` (built-in heading test) and `h` (an `xfuncname` driver) both hold
    /// headings ending in `\f`, `\v` and ` \f `, a 90-byte heading ending in `\f`,
    /// and a `\f`-only line between two sections for `-W`. Two commits edit them,
    /// a third is a merge, and the worktree carries one more edit.
    fn new(tag: &str) -> Option<Self> {
        let stock = stock_git::stock_git_at_least((2, 56, 0))?;
        let root = std::env::temp_dir().join(format!("zvcs-funcname-trailing-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let fx = Fixture { root, work, stock };
        let body = |n: &str| format!(" {n}1\n {n}2\n {n}3\n {n}4\n {n}5\n {n}6\n {n}7\n");
        let long = "x".repeat(90);
        let text = format!(
            "fa\x0c\n{}fb\x0b\n{}fc \x0c \n{}\x0c\n{long}\x0c\n{}",
            body("a"),
            body("b"),
            body("c"),
            body("d")
        );
        let w = |name: &str, text: &str| std::fs::write(fx.work.join(name), text).unwrap();
        fx.stock(&["init", "-q", "-b", "main", "."]);
        fx.stock(&["config", "diff.drv.xfuncname", "^[fx].*"]);
        w(".gitattributes", "h diff=drv\n");
        w("f", &text);
        w("h", &text);
        fx.stock(&["add", "."]);
        fx.stock(&["commit", "-q", "-m", "one"]);
        let edit = |from: &str, to: &str| {
            for name in ["f", "h"] {
                let p = fx.work.join(name);
                let t = std::fs::read_to_string(&p).unwrap().replace(from, to);
                std::fs::write(p, t).unwrap();
            }
        };
        edit(" a6\n", " A6\n");
        edit(" b6\n", " B6\n");
        edit(" c6\n", " C6\n");
        edit(" d6\n", " D6\n");
        fx.stock(&["commit", "-q", "-am", "two"]);
        fx.stock(&["checkout", "-q", "-b", "side", "HEAD~1"]);
        w("s", "s\n");
        fx.stock(&["add", "s"]);
        fx.stock(&["commit", "-q", "-m", "side"]);
        fx.stock(&["checkout", "-q", "main"]);
        fx.stock(&["merge", "-q", "--no-edit", "side"]);
        edit(" a5\n", " A5\n");
        edit(" c5\n", " C5\n");
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
fn history_verbs_keep_a_trailing_form_feed() {
    let Some(fx) = Fixture::new("history") else { return };
    for args in [
        &["log", "-p", "-1", "HEAD^"][..],
        &["log", "--cc", "-2"],
        &["log", "-m", "-p", "-1"],
        &["log", "-p", "-W", "-1", "HEAD^"],
        &["log", "-p", "--color=always", "-1", "HEAD^"],
        &["show", "HEAD^"],
        &["diff-tree", "-p", "HEAD^"],
        &["format-patch", "--stdout", "-1", "HEAD^"],
        &["format-patch", "--stdout", "-W", "-1", "HEAD^"],
    ] {
        fx.same(args);
    }
}

#[test]
fn worktree_diffs_keep_a_trailing_form_feed() {
    let Some(fx) = Fixture::new("worktree") else { return };
    for args in [
        &["diff"][..],
        &["diff", "-W"],
        &["diff", "HEAD"],
        &["diff-files", "-p"],
        &["diff-index", "-p", "HEAD"],
        &["diff", "HEAD~1", "HEAD"],
    ] {
        fx.same(args);
    }
}
