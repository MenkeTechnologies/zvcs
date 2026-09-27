//! `zworktree add` writes the new worktree's index through `write_locked_index()`.
//!
//! `git worktree add` populates the worktree with `git reset --hard`
//! (builtin/worktree.c:405). That index starts from nothing, so `do_write_index()`
//! picks its format from `index.version` (read-cache.c:2865-2866). zworktree
//! serialised the index straight into the file with default options, so a
//! repository configured for version 4 got a version 2 index in every worktree.
//!
//! Expectations measured from stock git 2.55.0 (`worktree add` with
//! `index.version=4` leaves `DIRC` version 4 in `.git/worktrees/<name>/index`).
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-zworktree-index-version-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let f = Fixture { root, repo };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.repo.join("a"), "a\n").unwrap();
        f.git(&["add", "a"]);
        f.git(&["commit", "-q", "-m", "one"]);
        f
    }

    fn git(&self, args: &[&str]) -> String {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.root)
            .env("ZVCS_HOME", self.root.join("zvcs-home"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@example.com")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@example.com")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

fn index_version(path: &Path) -> u32 {
    let bytes = std::fs::read(path).unwrap();
    assert_eq!(&bytes[..4], b"DIRC");
    u32::from_be_bytes(bytes[4..8].try_into().unwrap())
}

#[test]
fn a_new_worktree_index_takes_the_configured_version() {
    let f = Fixture::new();
    f.git(&["config", "index.version", "4"]);
    let dest = f.root.join("wt");
    f.git(&["zworktree", "add", "wt", dest.to_str().unwrap()]);

    assert_eq!(index_version(&f.repo.join(".git/worktrees/wt/index")), 4);
    // The worktree reads its own index back as clean.
    let status = Command::new(BIN)
        .args(["status", "--porcelain"])
        .current_dir(&dest)
        .env("HOME", &f.root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(status.status.success());
    assert_eq!(String::from_utf8_lossy(&status.stdout), "");
}
