//! `git status` holds `<index>.lock` across the whole collection.
//!
//! ```c
//! if (use_optional_locks())
//!         fd = repo_hold_locked_index(the_repository, &index_lock, 0);
//! else
//!         fd = -1;
//! …
//! wt_status_collect(&s);
//!
//! if (0 <= fd)
//!         repo_update_index_if_able(the_repository, &index_lock);
//! ```
//!
//! (builtin/commit.c:1634-1658.) The lock file exists while the untracked scan
//! runs, so an index kept inside the work tree (`GIT_INDEX_FILE=subidx`) has its
//! `subidx.lock` reported as untracked, and the racy-clean rewrite goes through
//! that same lock (`write_locked_index()`, read-cache.c:3309). A lock another
//! process holds is `fd < 0`: the report still prints and the index is left
//! alone. zvcs took the lock only inside gitoxide's index writer, at write time,
//! so the scan never saw it.
//!
//! Expectations measured from stock git 2.55.0.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A whole second well in the past; the file and the index are both stamped with it,
/// which makes the entry racy and `status` rewrite the index.
const PAST: &str = "202009131226.40";

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
    /// One committed file, `a`, and a copy of the index at `subidx` in the work tree.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-status-index-lock-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "base"]);
        std::fs::copy(f.work.join(".git/index"), f.work.join("subidx")).unwrap();
        let touched = Command::new("touch").args(["-t", PAST, "a", "subidx"]).current_dir(&f.work).status().unwrap();
        assert!(touched.success());
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
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
            .env("TZ", "UTC")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    /// Run with `GIT_INDEX_FILE=subidx`, relative to the top of the work tree.
    fn with_subidx(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("GIT_INDEX_FILE", "subidx")
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn subidx_rewritten(&self) -> bool {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(self.work.join("subidx")).unwrap().mtime() > 1_700_000_000
    }
}

#[test]
fn the_lock_is_listed_while_it_is_held_and_gone_after() {
    let f = Fixture::new("listed");
    for args in [&["status", "--porcelain"][..], &["status", "--short"], &["status", "--porcelain", "-uall"]] {
        assert_eq!(f.with_subidx(args), ("?? subidx\n?? subidx.lock\n".into(), String::new(), 0), "{args:?}");
    }
    assert_eq!(f.with_subidx(&["status", "--porcelain=v2"]), ("? subidx\n? subidx.lock\n".into(), String::new(), 0));
    assert!(!f.work.join("subidx.lock").exists());
}

#[test]
fn the_racy_rewrite_goes_through_the_held_lock() {
    let f = Fixture::new("racy");
    assert_eq!(f.with_subidx(&["status", "--porcelain"]).2, 0);
    assert!(f.subidx_rewritten(), "has_racy_timestamp() holds, so the index is rewritten");
    assert!(!f.work.join("subidx.lock").exists());
    assert_eq!(f.with_subidx(&["ls-files"]), ("a\n".into(), String::new(), 0));
}

#[test]
fn a_lock_someone_else_holds_skips_the_write() {
    let f = Fixture::new("foreign");
    std::fs::write(f.work.join("subidx.lock"), "").unwrap();
    assert_eq!(f.with_subidx(&["status", "--porcelain"]), ("?? subidx\n?? subidx.lock\n".into(), String::new(), 0));
    assert!(!f.subidx_rewritten());
    assert!(f.work.join("subidx.lock").exists());
}

#[test]
fn optional_locks_off_takes_no_lock() {
    let f = Fixture::new("optional");
    let out = Command::new(BIN)
        .args(["status", "--porcelain"])
        .current_dir(&f.work)
        .env("GIT_INDEX_FILE", "subidx")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("HOME", &f.root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "?? subidx\n");
    assert!(!f.subidx_rewritten());
}
