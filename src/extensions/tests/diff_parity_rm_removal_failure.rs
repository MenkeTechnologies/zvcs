//! `git rm` when one of the worktree files will not unlink.
//!
//! `cmd_rm()` states the rule in its own comment (builtin/rm.c:405-412):
//!
//! ```c
//! /*
//!  * Then, unless we used "--cached", remove the filenames from
//!  * the workspace. If we fail to remove the first one, we
//!  * abort the "git rm" (but once we've successfully removed
//!  * any file at all, we'll go ahead and commit to it all:
//!  * by then we've already committed ourselves and can't fail
//!  * in the middle)
//!  */
//! ```
//!
//! and the loop implements it with a `removed` latch, dying through
//! `die_errno("git rm: '%s'", path)` only while that latch is still clear
//! (builtin/rm.c:430-435). `remove_path()` itself treats `ENOENT` and `ENOTDIR`
//! as success (dir.c:3520-3525 via `is_missing_file_error()`), so a path that is
//! already gone never reaches the failure arm at all.
//!
//! The port died on the first `unlink()` refusal whatever had been removed
//! before it, which left the index unwritten — the paths were printed as removed
//! and then stayed staged. `unlink()` on a directory is the reachable case:
//! t4013-diff-various.sh's `-I<regex>` cleanup does exactly that (`mkdir file2`
//! over a staged `file2`), and its failure left the following twelve tests
//! looking at an index nobody expected.
//!
//! Every expectation was measured from stock git 2.55.0 over the same fixture.
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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-rm-fail-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("base"), "base\n").unwrap();
        f.git(&["add", "base"]);
        f.git(&["commit", "-q", "-m", "base"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("ZVCS_HOME", self.root.join("zvcs"))
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
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = self.cmd(args).output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    /// Stage `a`, `b` and `c`, then replace `dir`'s worktree file with a
    /// directory of the same name — a path `unlink()` refuses with `EPERM` on
    /// macOS and `EISDIR` on Linux, neither of which `is_missing_file_error()`
    /// forgives.
    ///
    /// Which name is chosen decides whether the refusal is the *first* one the
    /// removal meets: `cmd_rm()` walks the index, not the command line, so the
    /// order is always `a`, `b`, `c` however the arguments are spelled.
    fn stage_three_with_a_directory(&self, dir: &str) {
        for name in ["a", "b", "c"] {
            std::fs::write(self.work.join(name), "x\n").unwrap();
            self.git(&["add", name]);
        }
        std::fs::remove_file(self.work.join(dir)).unwrap();
        std::fs::create_dir(self.work.join(dir)).unwrap();
    }
}

/// The latch: `a` is removed first, so `b`'s refusal is swallowed and the whole
/// removal is still committed to — all three paths leave the index and the exit
/// status is zero.
#[test]
fn a_later_unlink_failure_does_not_abort_the_removal() {
    let f = Fixture::new("later");
    f.stage_three_with_a_directory("b");

    let (out, err, code) = f.run(&["rm", "-f", "a", "b", "c"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("rm 'a'\nrm 'b'\nrm 'c'\n", "", 0));

    let (tracked, _, _) = f.run(&["ls-files"]);
    assert_eq!(tracked, "base\n");
    // The directory that would not unlink is untouched, and `a`/`c` are gone.
    assert!(f.work.join("b").is_dir());
    assert!(!f.work.join("a").exists());
    assert!(!f.work.join("c").exists());
}

/// The other half of the same rule: when the path that will not unlink is the
/// one the index walk reaches first, nothing has been removed yet and `git rm`
/// dies before touching the index.
#[test]
fn the_first_unlink_failure_aborts_before_the_index_is_written() {
    let f = Fixture::new("first");
    f.stage_three_with_a_directory("a");

    let (out, err, code) = f.run(&["rm", "-f", "a", "b", "c"]);
    // The `rm '<path>'` report is written before anything is removed, so it is
    // complete even on the run that dies.
    assert_eq!(out, "rm 'a'\nrm 'b'\nrm 'c'\n");
    // `die_errno()` appends the C library's reason, which differs by platform.
    assert!(err.starts_with("fatal: git rm: 'a': "), "{err:?}");
    assert_eq!(code, 128);

    // Nothing left the index, and the two removable files are still there.
    let (tracked, _, _) = f.run(&["ls-files"]);
    assert_eq!(tracked, "a\nb\nbase\nc\n");
    assert!(f.work.join("b").exists());
    assert!(f.work.join("c").exists());
}

/// A path already missing from the worktree is not a failure at all
/// (`is_missing_file_error()`), so it both succeeds and arms the latch: `git rm`
/// over a deleted file alone still exits zero.
#[test]
fn an_already_missing_path_is_not_an_unlink_failure() {
    let f = Fixture::new("gone");
    std::fs::write(f.work.join("d"), "x\n").unwrap();
    f.git(&["add", "d"]);
    std::fs::remove_file(f.work.join("d")).unwrap();

    let (out, err, code) = f.run(&["rm", "-f", "d"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("rm 'd'\n", "", 0));
    let (tracked, _, _) = f.run(&["ls-files"]);
    assert_eq!(tracked, "base\n");
}
