//! `git add --refresh` and `git reset --mixed` run read-cache.c's own
//! `refresh_index()`, not a status walk.
//!
//! The two callers are one line each — `refresh_index(repo->index, flags,
//! pathspec, seen, _("Unstaged changes after refreshing the index:"))`
//! (builtin/add.c:133-134) and `refresh_index(the_repository->index, flags,
//! NULL, NULL, _("Unstaged changes after reset:"))` (builtin/reset.c:504-506) —
//! and both had been answered here by comparing the index against a worktree
//! status walk instead.
//!
//! That is not the same question. `ie_modified()` will not trust a size
//! difference when the recorded size is zero: "if the entry is racily clean, or
//! the size is not recorded, re-read the content" (read-cache.c:476-489). A
//! zeroed stat is exactly what `read-tree` and `reset --mixed`'s own
//! `read_from_tree()` leave behind, so the walk called each such entry modified,
//! the refresh declined to touch it, and the index kept a stat that made the
//! next `diff-files` report every one of those paths as changed.
//!
//! Three separate defects fell out of that, all covered below:
//!   * `git add --refresh` refreshed nothing at all and never wrote the index.
//!   * `git reset --mixed` left the entries it had just rebuilt unrefreshed.
//!   * `git reset --quiet` skipped the refresh entirely, where `--quiet` only
//!     picks `REFRESH_QUIET` over `REFRESH_IN_PORCELAIN` (builtin/reset.c:493).
//!
//! Every expectation was measured from stock git 2.55.0 in an identical
//! throwaway repository under the same pinned environment.
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
    /// One commit holding `file1` and `file2`, worktree clean.
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("zvcs-idx-refresh-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file1"), "one\n").unwrap();
        std::fs::write(f.work.join("file2"), "two\n").unwrap();
        f.git(&["add", "file1", "file2"]);
        f.git(&["commit", "-q", "-m", "files"]);
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
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn run(&self, args: &[&str]) -> (String, i32) {
        let out = self.cmd(args).output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    /// `git diff-files --name-only`, the question "which entries does the index
    /// still think are modified".
    fn dirty_paths(&self) -> String {
        self.run(&["diff-files", "--name-only"]).0
    }
}

/// t3700-add.sh's case 24: a `read-tree` zeroes every entry's stat, and
/// `git add --refresh` has to put it back — and persist it, since the refreshed
/// stat is worthless if it never reaches the index file.
#[test]
fn index_parity_add_refresh_restores_stat_after_read_tree() {
    let f = Fixture::new("addrefresh");
    f.git(&["read-tree", "HEAD"]);
    assert_eq!(
        f.dirty_paths(),
        "file1\nfile2\n",
        "read-tree was expected to leave both entries stat-stale"
    );

    let (out, code) = f.run(&["add", "--refresh", "--", "file1"]);
    assert_eq!((out.as_str(), code), ("", 0), "add --refresh spoke or failed");
    // Only the path the pathspec named is refreshed: `ce_path_match()` filters
    // the walk (read-cache.c:1546-1547).
    assert_eq!(f.dirty_paths(), "file2\n");

    // A bare `git add --refresh` names no pathspec, which is `cmd_add()`'s
    // "Nothing specified, nothing added." and refreshes nothing; `.` is what
    // reaches `refresh()` with a pathspec.
    f.git(&["add", "--refresh", "."]);
    assert_eq!(f.dirty_paths(), "");
}

/// Without `-v` the flag word is `REFRESH_QUIET`, with it `REFRESH_IN_PORCELAIN`
/// — which is the header plus `M\t<path>`, not `<path>: needs update`
/// (read-cache.c:1514-1518, and `show_file()` at :1450-1458).
#[test]
fn index_parity_add_refresh_reports_only_under_verbose() {
    let f = Fixture::new("addverbose");
    std::fs::write(f.work.join("file1"), "changed\n").unwrap();

    assert_eq!(f.run(&["add", "--refresh", "."]), (String::new(), 0));
    assert_eq!(
        f.run(&["add", "--refresh", "-v", "."]),
        (
            "Unstaged changes after refreshing the index:\nM\tfile1\n".to_string(),
            0
        ),
    );
}

/// t7102-reset.sh's cases 26-28. A `--mixed` reset rebuilds the entries it
/// replaces with no stat data, so without the refresh that follows, the paths it
/// just restored read back as modified.
#[test]
fn index_parity_reset_mixed_refreshes_the_entries_it_rebuilt() {
    let f = Fixture::new("resetmixed");

    // `git rm --cached` drops file2 from the index without touching the worktree;
    // the reset puts it back, and only a refresh can give it a usable stat.
    f.git(&["rm", "-q", "--cached", "file2"]);
    f.git(&["reset", "--mixed", "HEAD"]);
    assert_eq!(f.dirty_paths(), "", "reset --mixed left a stat-stale entry");

    // The pathspec form takes the same refresh, so resetting an unmodified path
    // is the no-op git documents.
    f.git(&["reset", "--hard"]);
    let (out, code) = f.run(&["reset", "--", "file1"]);
    assert_eq!((out.as_str(), code), ("", 0));
    assert_eq!(f.dirty_paths(), "");
}

/// `--quiet` chooses the flag word; it does not skip the refresh. `--no-refresh`
/// is the flag that skips it (builtin/reset.c:500).
#[test]
fn index_parity_reset_quiet_still_refreshes_and_no_refresh_does_not() {
    let f = Fixture::new("resetquiet");

    f.git(&["rm", "-q", "--cached", "file2"]);
    let (out, code) = f.run(&["reset", "--quiet", "--mixed", "HEAD"]);
    assert_eq!((out.as_str(), code), ("", 0), "--quiet reset spoke");
    assert_eq!(f.dirty_paths(), "", "--quiet skipped the refresh");

    f.git(&["rm", "-q", "--cached", "file2"]);
    let (out, code) = f.run(&["reset", "--no-refresh", "--mixed", "HEAD"]);
    assert_eq!((out.as_str(), code), ("", 0));
    assert_eq!(
        f.dirty_paths(),
        "file2\n",
        "--no-refresh refreshed the index anyway"
    );
}

/// The porcelain report a non-quiet `--mixed` reset prints for a path that is
/// genuinely modified, which the refresh must still not silence.
#[test]
fn index_parity_reset_mixed_names_unstaged_paths_under_its_header() {
    let f = Fixture::new("resetreport");
    std::fs::write(f.work.join("file1"), "changed\n").unwrap();
    f.git(&["add", "file1"]);

    let (out, code) = f.run(&["reset", "--mixed", "HEAD"]);
    assert_eq!(
        (out.as_str(), code),
        ("Unstaged changes after reset:\nM\tfile1\n", 0)
    );
}

/// `update-index --refresh` is the same walk and must keep its own spelling:
/// `<path>: needs update` with exit 1, and silence with exit 0 under `-q`
/// (the `if (quiet) continue;` above `has_errors = 1`, read-cache.c:1588-1601).
#[test]
fn index_parity_update_index_refresh_keeps_its_own_report_and_exit_code() {
    let f = Fixture::new("uirefresh");
    std::fs::write(f.work.join("file1"), "changed\n").unwrap();

    assert_eq!(
        f.run(&["update-index", "--refresh"]),
        ("file1: needs update\n".to_string(), 1)
    );
    assert_eq!(f.run(&["update-index", "-q", "--refresh"]), (String::new(), 0));
}
