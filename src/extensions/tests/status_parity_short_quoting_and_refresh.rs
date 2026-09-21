//! Two things `git status` does that only its short format (or the index on
//! disk) shows.
//!
//! **Quoting.** `quote_path(…, QUOTE_PATH_QUOTE_SP)` sets `force_dq` when the
//! relative path contains a space and then quotes with `CQUOTE_NODQ`
//! (quote.c:350-371). A space is not a character `quote_c_style()` escapes, so
//! that flag is the only reason such a path appears in quotes — and only the
//! three short-format printers pass it (wt-status.c:2037, :2066, :2071, :2086).
//! The long format and porcelain v2 pass `0` and leave the path bare.
//!
//! **Refresh.** `cmd_status()` runs `refresh_index(the_repository->index,
//! REFRESH_QUIET|REFRESH_UNMERGED|progress_flag, &s.pathspec, NULL, NULL)`
//! before collecting (builtin/commit.c:1628-1631), and
//! `repo_update_index_if_able()` writes what it dirtied (:1657). So a file whose
//! stat data went stale while its content did not is repaired by a plain `git
//! status`, and the next `git diff-files` says nothing about it. The write is
//! behind `use_optional_locks()` (:1635-1638) and behind a lock taken without
//! `LOCK_DIE_ON_ERROR`, so `GIT_OPTIONAL_LOCKS=0` and a read-only git directory
//! both leave the index alone while the report still prints.
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
        // The read-only case leaves a directory this process may not remove.
        let _ = std::process::Command::new("chmod")
            .args(["-R", "u+rwX", &self.root.to_string_lossy()])
            .status();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-st-shortq-{tag}-{}", std::process::id()));
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
            .env("ZVCS_HOME", &self.root)
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

    fn stdout(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

/// A space puts the path in quotes in `-s` and in porcelain v1, and leaves it
/// bare in porcelain v2 and the long format.
#[test]
fn a_space_is_quoted_only_by_the_short_formats() {
    let f = Fixture::new("space");
    std::fs::write(f.work.join("a file"), "x\n").unwrap();
    std::fs::write(f.work.join("plain"), "x\n").unwrap();
    f.git(&["add", "a file"]);

    assert_eq!(f.stdout(&["status", "-s"]), "A  \"a file\"\n?? plain\n");
    assert_eq!(f.stdout(&["status", "--porcelain"]), "A  \"a file\"\n?? plain\n");

    let v2 = f.stdout(&["status", "--porcelain=v2"]);
    assert!(v2.contains(" a file\n"), "v2 quoted a space:\n{v2}");
    let long = f.stdout(&["status"]);
    assert!(long.contains("\tnew file:   a file\n"), "long format quoted a space:\n{long}");
}

/// An untracked path with a space is quoted too, and a path needing real
/// C-quoting still gets exactly one pair of quotes.
#[test]
fn untracked_and_escaped_paths_keep_one_pair_of_quotes() {
    let f = Fixture::new("escape");
    std::fs::write(f.work.join("untracked file"), "x\n").unwrap();
    std::fs::write(f.work.join("tab\tand space"), "x\n").unwrap();
    let short = f.stdout(&["status", "-s"]);
    assert!(short.contains("?? \"untracked file\"\n"), "{short}");
    assert!(short.contains("?? \"tab\\tand space\"\n"), "{short}");
}

/// `-z` prints paths raw, quoting nothing at all.
#[test]
fn null_terminated_output_quotes_nothing() {
    let f = Fixture::new("nul");
    std::fs::write(f.work.join("a file"), "x\n").unwrap();
    f.git(&["add", "a file"]);
    assert_eq!(f.stdout(&["status", "-s", "-z"]), "A  a file\0");
}

/// A stat-dirty but content-identical file is repaired by `git status`, so the
/// `git diff-files` after it reports nothing.
#[test]
fn status_refreshes_the_index() {
    let f = Fixture::new("refresh");
    std::fs::write(f.work.join("file"), "content\n").unwrap();
    f.git(&["add", "file"]);
    f.git(&["commit", "-q", "-m", "one"]);
    // The index records whole-second mtimes, so the rewrite has to land in a
    // later second than the index write for the entry to look stale at all.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(f.work.join("file"), "content\n").unwrap();
    assert_ne!(f.stdout(&["diff-files"]), "", "the file was not stat-dirty to begin with");

    f.git(&["status"]);
    assert_eq!(f.stdout(&["diff-files"]), "", "status left the stat data stale");
}

/// `GIT_OPTIONAL_LOCKS=0` reports without touching the index.
#[test]
fn optional_locks_off_leaves_the_index_alone() {
    let f = Fixture::new("nolocks");
    std::fs::write(f.work.join("file"), "content\n").unwrap();
    f.git(&["add", "file"]);
    f.git(&["commit", "-q", "-m", "one"]);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(f.work.join("file"), "content\n").unwrap();
    let before = f.stdout(&["diff-files"]);
    assert_ne!(before, "");

    let out = f.cmd(&["status"]).env("GIT_OPTIONAL_LOCKS", "0").output().unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(f.stdout(&["diff-files"]), before, "the index was written anyway");
}

/// A git directory that cannot be written still gets a report: the lock is
/// taken without `LOCK_DIE_ON_ERROR`, so failing to take it is not an error.
#[test]
fn a_read_only_git_directory_still_reports() {
    let f = Fixture::new("readonly");
    std::fs::write(f.work.join("file"), "content\n").unwrap();
    f.git(&["add", "file"]);
    f.git(&["commit", "-q", "-m", "one"]);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(f.work.join("file"), "content\n").unwrap();

    use std::os::unix::fs::PermissionsExt as _;
    let git_dir = f.work.join(".git");
    let saved = std::fs::metadata(&git_dir).unwrap().permissions();
    std::fs::set_permissions(&git_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let out = f.cmd(&["status", "-s"]).output().unwrap();
    std::fs::set_permissions(&git_dir, saved).unwrap();

    assert!(
        out.status.success(),
        "status failed in a read-only repository: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stderr), "", "a refused lock was reported");
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("file"),
        "the report should still see the file as clean: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}
