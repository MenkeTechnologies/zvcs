//! `git describe --broken` settles its suffix from two forked children:
//!
//! ```c
//! if (broken) {
//!         strvec_pushv(&cp.args, update_index_args);
//!         ...
//!         run_command(&cp);
//!         ...
//!         strvec_pushv(&cp.args, diff_index_args);
//!         ...
//!         switch (run_command(&cp)) {
//!         case 0:  suffix = NULL;   break;
//!         case 1:  suffix = dirty;  break;
//!         default: suffix = broken;         /* diff-index aborted abnormally */
//!         }
//! ```
//! (`builtin/describe.c:726-755`, v2.56.0)
//!
//! From a cwd inside `.git` neither child has a work tree, so both die with
//! `fatal: this operation must be run in a work tree` on the inherited stderr and
//! describe appends the broken mark and exits 0. The port treated `--broken` as
//! `--dirty` and printed the bare name.
#![cfg(unix)]

use std::path::{Path, PathBuf};
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
    /// One commit of `f`, tagged `v1` (lightweight).
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-describe-broken-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.ok(&f.work, &["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("f"), "one\n").unwrap();
        f.ok(&f.work, &["add", "f"]);
        f.ok(&f.work, &["commit", "-q", "-m", "one"]);
        f.ok(&f.work, &["tag", "v1"]);
        f
    }

    fn run(&self, cwd: &Path, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().unwrap_or(-1),
        )
    }

    fn ok(&self, cwd: &Path, args: &[&str]) {
        let (_, err, code) = self.run(cwd, args);
        assert_eq!(code, 0, "`git {args:?}` failed: {err}");
    }
}

#[test]
fn children_without_a_work_tree_earn_the_broken_mark() {
    let f = Fixture::new("nowt");
    let objects = f.work.join(".git/objects");
    let wt = "fatal: this operation must be run in a work tree\n";
    assert_eq!(
        f.run(&objects, &["describe", "--tags", "--broken=-B"]),
        ("v1-B\n".into(), format!("{wt}{wt}"), 0)
    );
    // `--debug`'s line comes from describe() itself, after both children.
    assert_eq!(
        f.run(&objects, &["describe", "--tags", "--debug", "--broken"]).1.lines().collect::<Vec<_>>(),
        [wt.trim_end(), wt.trim_end(), "describe HEAD"]
    );
}

/// diff-index exiting 1 is the *dirty* mark, `--dirty`'s own one when given.
#[test]
fn a_dirty_tree_takes_the_dirty_mark_not_the_broken_one() {
    let f = Fixture::new("dirty");
    assert_eq!(f.run(&f.work, &["describe", "--tags", "--broken"]), ("v1\n".into(), String::new(), 0));
    std::fs::write(f.work.join("f"), "two\n").unwrap();
    assert_eq!(f.run(&f.work, &["describe", "--tags", "--broken"]), ("v1-dirty\n".into(), String::new(), 0));
    assert_eq!(
        f.run(&f.work, &["describe", "--tags", "--broken=-B", "--dirty=-D"]),
        ("v1-D\n".into(), String::new(), 0)
    );
}
