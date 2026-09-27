//! `zbump` writes the parent index through `do_write_index()`, racy smudge included.
//!
//! Staging a bumped gitlink rewrites the whole index, and every index write git makes
//! smudges the racy entries whose content moved (`do_write_index()`,
//! read-cache.c:2902-2903). zbump serialised the index directly, so an unrelated
//! entry — racily modified in the same second as the last index write — was written
//! back with its old size under a newer index timestamp, and from then on read as
//! clean: `status` and `diff-files` lost the change. The race is forced by stamping
//! the file and `.git/index` with the same past second.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

const PAST: &str = "202009131226.40";

struct Fixture {
    root: PathBuf,
    parent: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-zbump-racy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let parent = root.join("parent");
        let f = Fixture { root, parent };

        let sub_src = f.root.join("sub_src");
        std::fs::create_dir_all(&sub_src).unwrap();
        f.git(&sub_src, &["init", "-q", "-b", "main"]);
        f.git(&sub_src, &["commit", "--allow-empty", "-q", "-m", "s0"]);

        std::fs::create_dir_all(&f.parent).unwrap();
        f.git(&f.parent, &["init", "-q", "-b", "main"]);
        // The rewrite below moves ctime; keep it out of the stat comparison.
        f.git(&f.parent, &["config", "core.trustctime", "false"]);
        std::fs::write(f.parent.join("a"), "a\n").unwrap();
        f.stamp("a");
        f.git(&f.parent, &["add", "a"]);
        f.git(&f.parent, &["commit", "-q", "-m", "p0"]);
        f.git(&f.parent, &["submodule", "add", "-q", sub_src.to_str().unwrap(), "sub"]);
        f.git(&f.parent, &["commit", "-q", "-m", "add sub"]);
        // Advance the checked-out submodule past the recorded gitlink.
        f.git(&f.parent.join("sub"), &["commit", "--allow-empty", "-q", "-m", "s1"]);
        f
    }

    fn cmd(&self, dir: &Path, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(["-c", "protocol.file.allow=always"])
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("ZVCS_HOME", self.root.join("zvcs-home"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@example.com")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@example.com")
            .env("LC_ALL", "C");
        c
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let out = self.cmd(dir, args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn stamp(&self, path: &str) {
        let out = Command::new("touch").args(["-t", PAST, path]).current_dir(&self.parent).output().unwrap();
        assert!(out.status.success(), "touch failed: {out:?}");
    }

    fn recorded_size(&self, path: &str) -> String {
        let out = self.git(&self.parent, &["ls-files", "--debug", path]);
        let line = out.lines().find(|l| l.trim_start().starts_with("size:")).expect("size line");
        line.split_whitespace().nth(1).unwrap().to_owned()
    }
}

#[test]
fn an_unrelated_racily_modified_entry_is_smudged_by_the_bump_write() {
    let f = Fixture::new();
    // `a` was recorded with the past mtime; rewrite it same-size in that second.
    std::fs::write(f.parent.join("a"), "x\n").unwrap();
    f.stamp("a");
    f.stamp(".git/index");

    let out = f.git(&f.parent, &["zbump"]);
    assert!(out.contains("bumped sub:"), "zbump did not bump:\n{out}");

    assert_eq!(f.recorded_size("a"), "0");
    assert_eq!(f.git(&f.parent, &["diff-files", "--name-only"]), "a\n");
}
