//! `git submodule summary` and `git diff-index <tree>` over a path whose index
//! entries are unmerged stages mixing gitlinks and blobs.
//!
//! Without `--cached`, `do_oneway_diff()` (diff-lib.c:443-499) compares the
//! *first* stage — the entry `unpack_index_entry()` and `unpack_callback()`
//! hand over (unpack-trees.c:703-721, :1537-1561) — with the work tree as if it
//! were merged: `get_stat_data()` keeps the stage's own id when
//! `ce_compare_gitlink()` sees the checkout at it (or cannot read its `HEAD`),
//! and a null id with the stat mode otherwise, so a blob stage over a
//! submodule checkout reads as `160000` and a gitlink stage over a file as
//! `100644`. With no tree entry that is `show_new_file()`'s `A` pair, which
//! `submodule summary` turns into `* s0 0000000...<HEAD> (2):`. zvcs printed no
//! summary row at all for an unmerged gitlink `HEAD` lacked, and its
//! `diff-index` took stage 2 instead of the first stage.
//!
//! Every expectation is stock git's output for the same command on the same
//! fixture.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn command(bin: &str, dir: &Path, root: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(bin);
    c.args(args)
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
        .env("GIT_COMMITTER_DATE", "1700000000 +0000");
    c
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

/// The work tree state of `s0` once the index has its stages.
#[derive(Clone, Copy)]
enum Checkout {
    /// Checked out at the second submodule commit.
    AtTip,
    /// Checked out at the first submodule commit.
    AtFirst,
    /// An untracked file inside the checkout at the tip.
    Untracked,
    /// A freshly initialised repository with no commit.
    Unborn,
    /// A regular file where the checkout was.
    File,
    /// Nothing at all.
    Removed,
}

impl Fixture {
    /// A superproject with `f` committed, and `s0` an inner repository with
    /// commits `one` and `two`. `stages` lists the index entries written at
    /// `s0`: `1`/`3` the gitlink to `one`, `2` the gitlink to `two`, `b` a blob
    /// at stage 1, `B` a blob at stage 2, `T` a blob at stage 3. With
    /// `in_head`, `HEAD` records `s0` at `two` before the stages replace it.
    fn new(tag: &str, stages: &str, in_head: bool, checkout: Checkout) -> Option<Self> {
        let stock = stock_git::stock_git_at_least((2, 56, 0))?;
        let root = std::env::temp_dir().join(format!(
            "zvcs-sm-summary-unmerged-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let fx = Fixture { root, work, stock };
        let sub = fx.work.join("s0");
        fx.stock(&fx.work, &["init", "-q", "-b", "main", "."]);
        std::fs::write(fx.work.join("f"), "x\n").unwrap();
        fx.stock(&fx.work, &["add", "f"]);
        fx.stock(&fx.work, &["commit", "-qm", "base"]);
        std::fs::create_dir_all(&sub).unwrap();
        fx.stock(&sub, &["init", "-q", "-b", "main", "."]);
        std::fs::write(sub.join("a"), "a\n").unwrap();
        fx.stock(&sub, &["add", "a"]);
        fx.stock(&sub, &["commit", "-qm", "one"]);
        std::fs::write(sub.join("a"), "b\n").unwrap();
        fx.stock(&sub, &["commit", "-qam", "two"]);
        let two = fx.stock(&sub, &["rev-parse", "HEAD"]);
        let one = fx.stock(&sub, &["rev-parse", "HEAD~1"]);
        if in_head {
            fx.stock(&fx.work, &["add", "s0"]);
            fx.stock(&fx.work, &["commit", "-qm", "sub"]);
        }
        let mut info = String::new();
        for stage in stages.split_whitespace() {
            let line = match stage {
                "1" => format!("160000 {one} 1\ts0\n"),
                "2" => format!("160000 {two} 2\ts0\n"),
                "3" => format!("160000 {one} 3\ts0\n"),
                "b" => format!("100644 {} 1\ts0\n", fx.blob("base\n")),
                "B" => format!("100644 {} 2\ts0\n", fx.blob("blob\n")),
                "T" => format!("100644 {} 3\ts0\n", fx.blob("them\n")),
                other => panic!("unknown stage {other}"),
            };
            info.push_str(&line);
        }
        if in_head {
            fx.stock(&fx.work, &["update-index", "--force-remove", "s0"]);
        }
        fx.stock_stdin(&["update-index", "--index-info"], &info);
        match checkout {
            Checkout::AtTip => {}
            Checkout::AtFirst => {
                fx.stock(&sub, &["checkout", "-q", "HEAD~1"]);
            }
            Checkout::Untracked => std::fs::write(sub.join("u"), "u\n").unwrap(),
            Checkout::Unborn => {
                std::fs::remove_dir_all(&sub).unwrap();
                std::fs::create_dir_all(&sub).unwrap();
                fx.stock(&sub, &["init", "-q"]);
            }
            Checkout::File => {
                std::fs::remove_dir_all(&sub).unwrap();
                std::fs::write(&sub, "f\n").unwrap();
            }
            Checkout::Removed => std::fs::remove_dir_all(&sub).unwrap(),
        }
        Some(fx)
    }

    fn stock(&self, dir: &Path, args: &[&str]) -> String {
        let out = command(self.stock, dir, &self.root, args).output().unwrap();
        assert!(
            out.status.success(),
            "stock git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim_end().to_owned()
    }

    fn stock_stdin(&self, args: &[&str], input: &str) {
        use std::io::Write;
        let mut child = command(self.stock, &self.work, &self.root, args)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        assert!(child.wait().unwrap().success(), "stock git {args:?}");
    }

    fn blob(&self, text: &str) -> String {
        let mut child = command(self.stock, &self.work, &self.root, &["hash-object", "-w", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
        }
        let out = child.wait_with_output().unwrap();
        String::from_utf8_lossy(&out.stdout).trim_end().to_owned()
    }

    fn run(&self, bin: &str, args: &[&str]) -> Output {
        command(bin, &self.work, &self.root, args).output().unwrap()
    }

    /// Every command reads only, so both binaries run on the same fixture.
    fn assert_parity(&self, label: &str, args: &[&str]) {
        let want = self.run(self.stock, args);
        let got = self.run(BIN, args);
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
            "[{label}] git {args:?}"
        );
    }
}

const COMMANDS: &[&[&str]] = &[
    &["submodule", "summary"],
    &["submodule", "summary", "HEAD"],
    &["submodule", "summary", "--cached"],
    &["submodule", "summary", "-n", "1"],
    &["submodule", "summary", "--for-status"],
    &["submodule", "summary", "HEAD", "--", "s0"],
    &["submodule", "summary", "--files"],
    &["diff-index", "HEAD"],
    &["diff-index", "--raw", "HEAD"],
    &["diff-index", "-p", "HEAD"],
    &["diff-index", "--cached", "HEAD"],
    &["diff-index", "--cached", "-p", "HEAD"],
];

fn sweep(tag: &str, stages: &str, in_head: bool, checkouts: &[Checkout]) {
    for (i, &checkout) in checkouts.iter().enumerate() {
        let Some(fx) = Fixture::new(&format!("{tag}-{i}"), stages, in_head, checkout) else {
            return;
        };
        for args in COMMANDS {
            fx.assert_parity(&format!("{stages} head={in_head} checkout={i}"), args);
        }
    }
}

const ALL: &[Checkout] = &[
    Checkout::AtTip,
    Checkout::AtFirst,
    Checkout::Untracked,
    Checkout::Unborn,
    Checkout::File,
    Checkout::Removed,
];

/// `HEAD` lacks `s0`: the first stage is `show_new_file()`'s `A` pair, whose
/// id is null when the checkout moved off it and the stage's own otherwise.
#[test]
fn unmerged_gitlink_absent_from_head() {
    for stages in ["1 2 3", "2 3", "1 2", "2", "1 3", "3"] {
        sweep(&stages.replace(' ', ""), stages, false, ALL);
    }
}

/// Blob stages over a submodule checkout read as `160000` with a null id, and
/// a gitlink first stage under a blob later stage is still compared as one.
#[test]
fn unmerged_blob_and_gitlink_mix_absent_from_head() {
    for stages in ["B", "b 2", "1 T", "b 2 T", "B 3"] {
        sweep(&format!("mix{}", stages.replace(' ', "")), stages, false, ALL);
    }
}

/// `HEAD` records `s0` at `two`: `show_modified()` against the first stage, a
/// `D` pair when the checkout is gone and a `T` pair when a file replaced it.
#[test]
fn unmerged_gitlink_present_in_head() {
    for stages in ["1 2 3", "2", "1 3", "B"] {
        sweep(&format!("head{}", stages.replace(' ', "")), stages, true, ALL);
    }
}
