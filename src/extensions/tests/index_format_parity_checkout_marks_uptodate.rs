//! A file an `unpack_trees()` checkout has just written is up to date, so it is never racy.
//!
//! ```c
//! if (S_ISREG(st->st_mode)) {
//!         ce_mark_uptodate(ce);
//! ```
//!
//! (`fill_stat_cache_info()`, read-cache.c:200-203, reached through
//! `update_ce_after_write()` under `state.refresh_cache = 1`, unpack-trees.c:438.)
//! `is_racy_timestamp()` is only asked of an entry that is `!ce_uptodate(ce)`
//! (split-index.c:291-292, read-cache.c:2902), and the result index is dated by the one it
//! replaces (unpack-trees.c:1941-1942) — older than any file written now.
//!
//! Without the mark the file written this second counted as racily clean, and on the write
//! that folds everything into a new shared index the split half still carried a stand-in
//! for it, where stock's split half is empty.
//!
//! Measured on stock git 2.56.0: four 2001-dated files, `side` changing `f3`, a fresh
//! split, then `reset -q --hard side` — one refilled entry in four crosses the 20% default,
//! a second shared index is written, and the split half holds `n=0` entries.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, UNIX_EPOCH};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// 2001-09-09T01:46:40Z.
const PAST: u64 = 1_000_000_000;

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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-uptodate-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.ok(&["init", "-q", "-b", "main", "."]);
        for i in 1..=4u32 {
            f.write(&format!("f{i}"), &format!("{i}\n"));
        }
        f.ok(&["add", "."]);
        f.ok(&["commit", "-q", "-m", "a"]);
        f.ok(&["checkout", "-q", "-b", "side"]);
        f.write("f3", "side3\n");
        f.ok(&["commit", "-q", "-a", "-m", "s"]);
        f.ok(&["checkout", "-q", "main"]);
        f.date("f3");
        f.ok(&["update-index", "-q", "--really-refresh"]);
        f.ok(&["update-index", "--split-index"]);
        f
    }

    fn ok(&self, args: &[&str]) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env_remove("GIT_INDEX_VERSION")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@example.com")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@example.com")
            .output()
            .unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn write(&self, rel: &str, content: &str) {
        std::fs::write(self.work.join(rel), content).unwrap();
        self.date(rel);
    }

    fn date(&self, rel: &str) {
        std::fs::File::options()
            .write(true)
            .open(self.work.join(rel))
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_secs(PAST))
            .unwrap();
    }

    fn split_half_entries(&self) -> u32 {
        let index = std::fs::read(self.work.join(".git/index")).unwrap();
        assert_eq!(&index[..4], b"DIRC");
        u32::from_be_bytes(index[8..12].try_into().unwrap())
    }

    fn shared_indexes(&self) -> usize {
        std::fs::read_dir(self.work.join(".git"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with("sharedindex."))
            .count()
    }
}

#[test]
fn a_hard_reset_leaves_no_stand_in_for_the_file_it_wrote() {
    let f = Fixture::new("reset");
    f.ok(&["reset", "-q", "--hard", "side"]);
    assert_eq!(f.shared_indexes(), 2, "the refilled f3 crosses the 20% default");
    assert_eq!(f.split_half_entries(), 0, "f3 was just written, so it is up to date and not racy");
}

#[test]
fn a_read_tree_update_leaves_no_stand_in_for_the_file_it_wrote() {
    let f = Fixture::new("readtree");
    f.ok(&["read-tree", "-m", "-u", "HEAD", "side"]);
    assert_eq!(f.shared_indexes(), 2);
    assert_eq!(f.split_half_entries(), 0);
}
