//! `update-index --refresh` rewrites a racy index, and that write must smudge.
//!
//! ```c
//! if (has_racy_timestamp(the_repository->index)) {
//!         ...
//!         the_repository->index->cache_changed |= SOMETHING_CHANGED;
//! }
//! ```
//!
//! (builtin/update-index.c:740-750.) A refresh that finds any racy entry writes the
//! index even when nothing changed, and the write goes through `do_write_index()`,
//! which smudges every racy entry that is not `ce_uptodate()` and whose content moved
//! (read-cache.c:2902-2903, `ce_smudge_racily_clean_entry()` at :2560). The entry the
//! refresh just reported as modified is not up to date, so it leaves with size 0.
//!
//! zvcs wrote that index without the smudge. The rewritten index is newer than the
//! entry's mtime, so the entry stopped looking racy while its size and mtime still
//! matched the file: `diff-files` then called it clean. With an ordinary clock that
//! took a second boundary between the index write and the refresh (7/300 runs of
//! `add; rewrite; update-index -q --refresh; diff-files --quiet`); here the race is
//! forced by stamping the file and `.git/index` with the same past second, so the
//! refresh always writes at a later second.
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
        let root = std::env::temp_dir().join(format!("zvcs-ui-racy-refresh-{tag}-{}", std::process::id()));
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

    /// `a` staged as `a\n`, then rewritten with `body` (same length). The file and the
    /// index both carry [`PAST`], so the entry is racy and its mtime and size match.
    fn racy_rewrite(&self, body: &str, move_ctime: bool) {
        std::fs::write(self.work.join("a"), "a\n").unwrap();
        self.stamp("a");
        self.git(&["add", "a"]);
        if move_ctime {
            std::thread::sleep(std::time::Duration::from_millis(1100));
        }
        std::fs::write(self.work.join("a"), body).unwrap();
        self.stamp("a");
        self.stamp(".git/index");
    }

    fn recorded_size(&self) -> String {
        let out = self.git(&["ls-files", "--debug", "a"]);
        let line = out.lines().find(|l| l.trim_start().starts_with("size:")).expect("size line");
        line.split_whitespace().nth(1).unwrap().to_owned()
    }
}

#[test]
fn a_racily_modified_entry_is_smudged_by_the_refresh_write() {
    let f = Fixture::new("smudge");
    // Keeps the rewrite's new ctime out of the stat comparison, so size and mtime
    // really are all the stat has to go on.
    f.git(&["config", "core.trustctime", "false"]);
    f.racy_rewrite("x\n", false);
    assert_eq!(f.run(&["update-index", "-q", "--refresh"]), (String::new(), 0));
    assert_eq!(f.recorded_size(), "0");
    assert_eq!(f.run(&["diff-files", "--quiet"]).1, 1);
    assert_eq!(f.git(&["diff-files", "--name-only"]), "a\n");
}

/// The non-quiet refresh reports the path and exits 1, and still writes the smudge.
#[test]
fn a_reporting_refresh_smudges_too() {
    let f = Fixture::new("report");
    f.git(&["config", "core.trustctime", "false"]);
    f.racy_rewrite("x\n", false);
    assert_eq!(f.run(&["update-index", "--refresh"]), ("a: needs update\n".into(), 1));
    assert_eq!(f.recorded_size(), "0");
    assert_eq!(f.run(&["diff-files", "--quiet"]).1, 1);
}

/// Racy but unchanged: `ie_match_stat()` checks the content, finds it equal and
/// marks the entry up to date (read-cache.c:1406-1420), so the write leaves it be.
#[test]
fn a_racily_clean_entry_with_unchanged_content_keeps_its_size() {
    let f = Fixture::new("clean");
    f.git(&["config", "core.trustctime", "false"]);
    f.racy_rewrite("a\n", false);
    assert_eq!(f.run(&["update-index", "-q", "--refresh"]), (String::new(), 0));
    assert_eq!(f.recorded_size(), "2");
    assert_eq!(f.run(&["diff-files", "--quiet"]).1, 0);
}

/// Under the default `core.trustctime=true` a ctime in a later second is already a
/// stat difference: `ce_match_stat_basic()` returns non-zero and
/// `ce_smudge_racily_clean_entry()` leaves the size alone (read-cache.c:2575).
#[test]
fn a_ctime_change_is_reported_by_stat_and_not_smudged() {
    let f = Fixture::new("ctime");
    f.racy_rewrite("x\n", true);
    assert_eq!(f.run(&["update-index", "-q", "--refresh"]), (String::new(), 0));
    assert_eq!(f.recorded_size(), "2");
    assert_eq!(f.run(&["diff-files", "--quiet"]).1, 1);
}
