//! `git diff-index -p <tree>` over a pair whose `S_IFMT` bits differ: `run_diff()`
//! (diff.c:5031-5049) splits it into a deletion of the old side and a creation of
//! the new one, two `diff --git` sections for one path, while `--raw` and the stat
//! family keep the single `T` pair. zvcs printed one section with `old mode` /
//! `new mode` and a hunk across the two types.
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
    /// `f` a file that becomes a symlink, `l` a symlink that becomes a file, `s`
    /// a submodule checkout replaced by a file, and `m` an ordinary edit, all in
    /// the work tree; `c` is staged as a symlink over a committed file.
    fn new(tag: &str) -> Option<Self> {
        let stock = stock_git::stock_git_at_least((2, 56, 0))?;
        let root = std::env::temp_dir().join(format!("zvcs-diff-index-typechange-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let fx = Fixture { root, work, stock };
        let w = |name: &str, text: &str| std::fs::write(fx.work.join(name), text).unwrap();
        fx.stock(&fx.work, &["init", "-q", "-b", "main", "."]);
        w("f", "one\ntwo\n");
        w("m", "a\n");
        w("c", "c\n");
        std::os::unix::fs::symlink("target", fx.work.join("l")).unwrap();
        let sub = fx.work.join("s");
        std::fs::create_dir_all(&sub).unwrap();
        fx.stock(&sub, &["init", "-q", "-b", "main", "."]);
        std::fs::write(sub.join("a"), "a\n").unwrap();
        fx.stock(&sub, &["add", "a"]);
        fx.stock(&sub, &["commit", "-qm", "one"]);
        fx.stock(&fx.work, &["add", "f", "m", "c", "l", "s"]);
        fx.stock(&fx.work, &["commit", "-qm", "base"]);
        std::fs::remove_file(fx.work.join("f")).unwrap();
        std::os::unix::fs::symlink("elsewhere", fx.work.join("f")).unwrap();
        std::fs::remove_file(fx.work.join("l")).unwrap();
        w("l", "now a file\n");
        std::fs::remove_dir_all(&sub).unwrap();
        w("s", "not a submodule\n");
        w("m", "b\n");
        std::fs::remove_file(fx.work.join("c")).unwrap();
        std::os::unix::fs::symlink("c-target", fx.work.join("c")).unwrap();
        fx.stock(&fx.work, &["add", "c"]);
        Some(fx)
    }

    fn stock(&self, dir: &Path, args: &[&str]) {
        let out = run(self.stock, dir, &self.root, args);
        assert!(
            out.status.success(),
            "stock git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn assert_parity(&self, args: &[&str]) {
        let want = run(self.stock, &self.work, &self.root, args);
        let got = run(BIN, &self.work, &self.root, args);
        assert_eq!(
            (
                String::from_utf8_lossy(&got.stdout),
                String::from_utf8_lossy(&got.stderr),
                got.status.code()
            ),
            (
                String::from_utf8_lossy(&want.stdout),
                String::from_utf8_lossy(&want.stderr),
                want.status.code()
            ),
            "git {args:?}"
        );
    }
}

#[test]
fn type_change_patch_is_a_deletion_then_a_creation() {
    let Some(fx) = Fixture::new("split") else { return };
    for args in [
        &["diff-index", "-p", "HEAD"][..],
        &["diff-index", "--cached", "-p", "HEAD"],
        &["diff-index", "-R", "-p", "HEAD"],
        &["diff-index", "-p", "--stat", "HEAD"],
        &["diff-index", "--patch-with-raw", "HEAD"],
        &["diff-index", "-p", "--full-index", "HEAD"],
        &["diff-index", "HEAD"],
        &["diff-index", "--numstat", "HEAD"],
    ] {
        fx.assert_parity(args);
    }
}
