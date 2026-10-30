//! `git diff-tree --cc --color=always <merge>` paints the combined patch.
//! `show_patch_diff()` hands `dump_sline()` `opt->use_color` (combine-diff.c:1210),
//! which `--color[=<when>]`/`--no-color` set; `diff-tree` loads
//! `git_diff_basic_config` (builtin/diff-tree.c:127), so `color.diff.<slot>` repaints
//! it while `color.diff`/`color.ui` cannot turn it on. zvcs printed the combined
//! patch uncoloured whatever was asked.
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
    /// A merge whose conflicted `f` (with a hunk-comment line above the change)
    /// and `g` were resolved to text neither side had, plus `s`, which only the
    /// side branch touched, so it is not part of the combined diff.
    fn new(tag: &str) -> Option<Self> {
        let stock = stock_git::stock_git_at_least((2, 56, 0))?;
        let root = std::env::temp_dir().join(format!("zvcs-diff-tree-cc-color-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let fx = Fixture { root, work, stock };
        let w = |name: &str, text: &str| std::fs::write(fx.work.join(name), text).unwrap();
        fx.stock(&["init", "-q", "-b", "main", "."]);
        w("f", "fn a\n1\n2\n3\n4\n5\n6\n");
        w("g", "base\n");
        fx.stock(&["add", "."]);
        fx.stock(&["commit", "-q", "-m", "base"]);
        fx.stock(&["checkout", "-q", "-b", "side"]);
        w("f", "fn a\n1\n2\n3\n4\nside\n6\n");
        w("g", "side\n");
        w("s", "s\n");
        fx.stock(&["add", "."]);
        fx.stock(&["commit", "-q", "-m", "side"]);
        fx.stock(&["checkout", "-q", "main"]);
        w("f", "fn a\n1\n2\n3\n4\nmain\n6\n");
        w("g", "main\n");
        fx.stock(&["commit", "-q", "-am", "main"]);
        let _ = run(stock, &fx.work, &fx.root, &["merge", "-q", "side"]);
        w("f", "fn a\n1\n2\n3\n4\nresolved\n6\n");
        w("g", "res\n");
        fx.stock(&["add", "f", "g"]);
        fx.stock(&["commit", "-q", "-m", "merge"]);
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
fn the_combined_patch_takes_the_command_line_colour() {
    let Some(fx) = Fixture::new("when") else { return };
    for args in [
        &["diff-tree", "--cc", "--color=always", "HEAD"][..],
        &["diff-tree", "--cc", "--color", "HEAD"],
        &["diff-tree", "-c", "-p", "--color=always", "HEAD"],
        &["diff-tree", "--cc", "--combined-all-paths", "--color=always", "HEAD"],
        &["diff-tree", "--cc", "--stat", "-p", "--color=always", "HEAD"],
        &["diff-tree", "--cc", "--raw", "-p", "--color=always", "HEAD"],
        &["diff-tree", "--cc", "--line-prefix=> ", "--color=always", "HEAD"],
        &["diff-tree", "--cc", "--color=always", "--no-color", "HEAD"],
        &["diff-tree", "--cc", "--no-color", "--color", "HEAD"],
        &["diff-tree", "--cc", "--color=never", "HEAD"],
    ] {
        fx.same(args);
    }
}

#[test]
fn slot_colours_repaint_but_cannot_enable() {
    let Some(fx) = Fixture::new("slots") else { return };
    for args in [
        &["-c", "color.diff.meta=blue", "-c", "color.diff.frag=yellow bold", "diff-tree", "--cc", "--color=always", "HEAD"][..],
        &["-c", "color.diff.func=red", "-c", "color.diff.new=cyan", "diff-tree", "--cc", "--color=always", "HEAD"],
        &["-c", "color.diff=always", "diff-tree", "--cc", "HEAD"],
        &["-c", "color.ui=always", "diff-tree", "--cc", "HEAD"],
    ] {
        fx.same(args);
    }
}
