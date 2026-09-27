//! `receive.denyCurrentBranch=updateInstead` must see a racily-modified worktree file.
//!
//! `push_to_deploy()` (builtin/receive-pack.c:1388) runs, in the pushed-to worktree:
//!
//! ```c
//! "update-index", "-q", "--ignore-submodules", "--refresh"   -> "Up-to-date check failed"
//! "diff-files", "--quiet", "--ignore-submodules", "--"       -> "Working directory has unstaged changes"
//! ```
//!
//! The refresh rewrites a racy index (builtin/update-index.c:740-750), and that write
//! smudges the entry whose content moved (`do_write_index()`, read-cache.c:2902-2903).
//! zvcs's `update-index` wrote without the smudge, so the rewritten index was newer
//! than the entry, `diff-files` trusted the matching stat, the push was accepted and
//! `read-tree -u -m` ran over the uncommitted edit (about 1 push in 8 with a real
//! clock). The fixture forces the race by stamping the remote's file and index with
//! the same past second.
//!
//! Expectations measured from stock git 2.55.0.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

const PAST: &str = "202009131226.40";

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-rp-racy-update-instead-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("home")).unwrap();
        Fixture { root }
    }

    fn run(&self, dir: &Path, args: &[&str]) -> (String, String, i32) {
        // `push` reaches `receive-pack` through `PATH`, so put this binary first.
        let bin_dir = Path::new(BIN).parent().unwrap();
        let path = format!("{}:{}", bin_dir.display(), std::env::var("PATH").unwrap_or_default());
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env("PATH", path)
            .env("HOME", self.root.join("home"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let (out, err, code) = self.run(dir, args);
        assert_eq!(code, 0, "`git {args:?}` failed: {err}");
        out
    }

    fn stamp(&self, dir: &Path, path: &str) {
        let out = Command::new("touch").args(["-t", PAST, path]).current_dir(dir).output().unwrap();
        assert!(out.status.success(), "touch failed: {out:?}");
    }
}

#[test]
fn a_racy_same_size_edit_in_the_remote_worktree_refuses_the_push() {
    let f = Fixture::new();
    let rem = f.root.join("rem");
    let loc = f.root.join("loc");
    f.git(&f.root, &["init", "-q", "-b", "main", "rem"]);
    // The edit below moves ctime; keep it out of the comparison so only the racy
    // rule can notice the change.
    f.git(&rem, &["config", "core.trustctime", "false"]);
    f.git(&rem, &["config", "receive.denyCurrentBranch", "updateInstead"]);
    std::fs::write(rem.join("a"), "a\n").unwrap();
    f.stamp(&rem, "a");
    f.git(&rem, &["add", "a"]);
    f.git(&rem, &["commit", "-q", "-m", "one"]);
    let before = f.git(&rem, &["rev-parse", "HEAD"]);

    f.git(&f.root, &["clone", "-q", "rem", "loc"]);
    std::fs::write(loc.join("b"), "b\n").unwrap();
    f.git(&loc, &["add", "b"]);
    f.git(&loc, &["commit", "-q", "-m", "two"]);

    // Uncommitted, same size, same mtime, and in the same second as the index.
    std::fs::write(rem.join("a"), "x\n").unwrap();
    f.stamp(&rem, "a");
    f.stamp(&rem, ".git/index");

    let (_, err, code) = f.run(&loc, &["push", "origin", "main"]);
    assert_eq!(code, 1, "{err}");
    assert!(
        err.contains(" ! [remote rejected] main -> main (Working directory has unstaged changes)\n"),
        "{err}"
    );
    assert_eq!(std::fs::read_to_string(rem.join("a")).unwrap(), "x\n");
    assert_eq!(f.git(&rem, &["rev-parse", "HEAD"]), before);
    assert!(!rem.join("b").exists());
}
