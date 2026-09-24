//! The on-disk version a `read-tree` writes.
//!
//! `unpack_trees()` starts its result from `o->src_index->version`
//! (unpack-trees.c:1940), and `do_write_index()` only consults `GIT_INDEX_VERSION` /
//! `index.version` when that is still zero (read-cache.c:2865-2866). So `--reset`,
//! `-m` and the two-tree merge — which read the index first (builtin/read-tree.c:236)
//! — keep the version it was in, while a plain `read-tree <tree>`, and a merge-like
//! read over an index that does not exist yet, take the requested one.
//!
//! Expectations measured against stock git 2.55.0.
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
    /// Two commits of `a`, leaving a version 2 index.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-rtver-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        for body in ["a\n", "a\nb\n"] {
            std::fs::write(f.work.join("a"), body).unwrap();
            f.git(&["add", "a"]);
            f.git(&["commit", "-q", "-m", body]);
        }
        assert_eq!(f.version(), 2);
        f
    }

    fn git(&self, args: &[&str]) {
        self.run(args, &[]);
    }

    /// `args` with `GIT_INDEX_VERSION=4` in the environment.
    fn git_v4(&self, args: &[&str]) {
        self.run(args, &[("GIT_INDEX_VERSION", "4")]);
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .envs(env.iter().copied())
            .output()
            .unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    /// The version word of `.git/index`'s header.
    fn version(&self) -> u32 {
        let bytes = std::fs::read(self.work.join(".git/index")).unwrap();
        u32::from_be_bytes(bytes[4..8].try_into().unwrap())
    }
}

#[test]
fn merge_like_reads_keep_the_version_they_read() {
    let f = Fixture::new("keep");
    for args in [
        &["read-tree", "--reset", "HEAD"][..],
        &["read-tree", "-m", "HEAD"],
        &["read-tree", "-m", "HEAD^", "HEAD"],
    ] {
        f.git_v4(args);
        assert_eq!(f.version(), 2, "{args:?}");
    }
}

#[test]
fn a_state_with_no_version_takes_the_requested_one() {
    let f = Fixture::new("fresh");
    f.git_v4(&["read-tree", "HEAD"]);
    assert_eq!(f.version(), 4, "a plain read never read the old index");

    std::fs::remove_file(f.work.join(".git/index")).unwrap();
    f.git_v4(&["read-tree", "--reset", "HEAD"]);
    assert_eq!(f.version(), 4, "an absent index has no version to carry");
}
