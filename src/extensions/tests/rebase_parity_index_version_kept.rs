//! A rebase rewrites the index in the version it had, not in `index.version`.
//!
//! The replay checks trees out through `unpack_trees()`, which sets the result's version from the
//! source index (`o->internal.result.version = o->internal.src_index->version`, unpack-trees.c).
//! zvcs built a fresh index from the target tree and so wrote `index.version`'s 4 over a version 2
//! file. Expectations measured from stock git 2.56.0.

use std::path::PathBuf;
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

impl Fixture {
    /// `main` is one commit ahead of `base`; `side` branches from `base` with its own file.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-rebase-version-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        f.run(&[], &["init", "-q", "-b", "main", "."]);
        for (file, branch) in [("a", "main"), ("b", "side"), ("c", "main")] {
            if branch == "side" {
                f.run(&[], &["checkout", "-q", "-b", "side"]);
            } else if file == "c" {
                f.run(&[], &["checkout", "-q", "main"]);
            }
            std::fs::write(f.root.join(file), format!("{file}\n")).unwrap();
            f.run(&[], &["add", file]);
            f.run(&[], &["commit", "-q", "-m", file]);
        }
        f.run(&[], &["checkout", "-q", "side"]);
        f
    }

    fn run(&self, config: &[&str], args: &[&str]) -> i32 {
        let mut cmd = Command::new(BIN);
        for c in config {
            cmd.args(["-c", c]);
        }
        cmd.args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .output()
            .unwrap()
            .status
            .code()
            .expect("no signal")
    }

    fn index_version(&self) -> u32 {
        let bytes = std::fs::read(self.root.join(".git/index")).unwrap();
        u32::from_be_bytes(bytes[4..8].try_into().unwrap())
    }
}

#[test]
fn replaying_onto_a_new_base_keeps_the_version_of_the_index_it_replaces() {
    let f = Fixture::new("replay");
    assert_eq!(f.index_version(), 2);
    assert_eq!(f.run(&["index.version=4"], &["rebase", "main"]), 0);
    assert_eq!(f.index_version(), 2);
}

#[test]
fn a_fast_forward_of_the_branch_keeps_it_too() {
    let f = Fixture::new("ff");
    assert_eq!(f.run(&[], &["checkout", "-q", "base"]), 1, "no such branch, state untouched");
    f.run(&[], &["checkout", "-q", "main"]);
    f.run(&[], &["branch", "-f", "behind", "HEAD~1"]);
    f.run(&[], &["checkout", "-q", "behind"]);
    assert_eq!(f.run(&["index.version=4"], &["rebase", "main"]), 0);
    assert_eq!(f.index_version(), 2);
}
