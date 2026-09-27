//! Which racily-clean entries an index write smudges.
//!
//! ```c
//! if (!ce_uptodate(ce) && is_racy_timestamp(istate, ce))
//!         ce_smudge_racily_clean_entry(istate, ce);
//! ```
//!
//! (`do_write_index()`, read-cache.c:2902-2903.) The smudge only zeroes an entry
//! whose stat *still matches* the file — `if (ce_match_stat_basic(ce, &st))
//! return;` (read-cache.c:2575) — and whose content does not
//! (`ce_modified_check_fs()`). `ce_match_stat_basic()` compares the whole of
//! `match_stat_data()` (statinfo.c:64-104), ctime included unless
//! `core.trustctime=false`. zvcs compared only size and mtime, so a rewrite that
//! moved ctime into a later second was smudged where stock leaves the recorded
//! size alone.
//!
//! The race is forced rather than waited for: the file and `.git/index` are both
//! stamped with the same past mtime, so `istate->timestamp.sec <= sd_mtime.sec`
//! holds for the entry, and `status` (which writes the index whenever
//! `has_racy_timestamp()` holds, builtin/commit.c:1655-1658) rewrites it at the
//! current time — the moment the entry stops looking racy unless it was smudged.
//!
//! Expectations measured from stock git 2.55.0.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A whole second well in the past; the file and the index are both stamped with it.
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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-status-racy-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C");
        c
    }

    fn run(&self, args: &[&str]) -> (String, i32) {
        let out = self.cmd(args).output().unwrap();
        (String::from_utf8_lossy(&out.stdout).into_owned(), out.status.code().expect("no signal"))
    }

    fn git(&self, args: &[&str]) -> String {
        let (out, code) = self.run(args);
        assert_eq!(code, 0, "`git {args:?}` failed");
        out
    }

    fn stamp(&self, path: &str) {
        let out = Command::new("touch").args(["-t", PAST, path]).current_dir(&self.work).output().unwrap();
        assert!(out.status.success(), "touch failed: {out:?}");
    }

    /// `a` staged as `a\n`, then rewritten to the same length. Both the file and the
    /// index carry [`PAST`] as their mtime, so the entry is racy and its recorded
    /// mtime and size still match the file.
    fn racy_rewrite(&self, move_ctime: bool) {
        std::fs::write(self.work.join("a"), "a\n").unwrap();
        self.stamp("a");
        self.git(&["add", "a"]);
        if move_ctime {
            // The rewrite's ctime lands in a later second than the one `add` recorded.
            std::thread::sleep(std::time::Duration::from_millis(1100));
        }
        std::fs::write(self.work.join("a"), "x\n").unwrap();
        self.stamp("a");
        self.stamp(".git/index");
    }

    /// The `size:` field `ls-files --debug` prints for `a`.
    fn recorded_size(&self) -> String {
        let out = self.git(&["ls-files", "--debug", "a"]);
        let line = out.lines().find(|l| l.trim_start().starts_with("size:")).expect("size line");
        line.split_whitespace().nth(1).unwrap().to_owned()
    }
}

/// `core.trustctime=true` (the default) and a ctime in a later second:
/// `ce_match_stat_basic()` already reports `CTIME_CHANGED`, so the write leaves
/// `sd_size` at 2 and the stat difference is what keeps the path dirty.
#[test]
fn a_ctime_change_is_a_stat_difference_and_is_not_smudged() {
    let f = Fixture::new("ctime");
    f.racy_rewrite(true);
    assert_eq!(f.git(&["status", "--porcelain"]), "AM a\n");
    assert_eq!(f.recorded_size(), "2");
    assert_eq!(f.git(&["status", "--porcelain"]), "AM a\n");
    assert_eq!(f.run(&["diff-files", "--quiet"]).1, 1);
}

/// With ctime out of the comparison the stat matches completely, only the content
/// differs, and the write zeroes the size so every later command re-reads it.
#[test]
fn a_full_stat_match_with_moved_content_is_smudged() {
    let f = Fixture::new("smudge");
    f.git(&["config", "core.trustctime", "false"]);
    f.racy_rewrite(false);
    assert_eq!(f.git(&["status", "--porcelain"]), "AM a\n");
    assert_eq!(f.recorded_size(), "0");
    assert_eq!(f.git(&["status", "--porcelain"]), "AM a\n");
    assert_eq!(f.run(&["diff-files", "--quiet"]).1, 1);
}
