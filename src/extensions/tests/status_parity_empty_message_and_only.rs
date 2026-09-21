//! What counts as an empty commit message, and what `--only` commits when it
//! names no path.
//!
//! `message_is_empty()` is `rest_is_empty(sb, 0)` (sequencer.c:1229-1233), and
//! `rest_is_empty()` walks the buffer line by line skipping anything that starts
//! with `sign_off_header` and anything that is all whitespace (:1186-1210). A
//! message of nothing but `Signed-off-by:` lines is therefore empty, and refused
//! without `--allow-empty-message`.
//!
//! `prepare_index()` takes its as-is branch only for `if (!only &&
//! !pathspec.nr)` (builtin/commit.c:481); every other non-`-a`, non-`-i` case is
//! `COMMIT_PARTIAL` (:516), which builds a false index from
//! `create_base_index(current_head)` — HEAD's tree — plus the paths
//! `list_paths()` matched. With `--only` and no path there are none, so the
//! commit records HEAD's tree and the index is left alone. That combination is
//! rejected outright unless `--amend` or `--allow-empty` asked for it (:390-392).
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
    /// One commit holding `file` with the content `committed`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-st-empty-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "committed\n").unwrap();
        f.git(&["add", "file"]);
        f.git(&["commit", "-q", "-m", "one"]);
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
            .env("GIT_EDITOR", ":")
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

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = self.cmd(args).output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn stdout(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// The blob HEAD records for `file`.
    fn committed_file(&self) -> String {
        self.stdout(&["cat-file", "blob", "HEAD:file"])
    }
}

/// A message that is only sign-offs and blanks is an empty message.
#[test]
fn a_message_of_only_sign_offs_is_empty() {
    let f = Fixture::new("signoff");
    std::fs::write(f.work.join("file"), "changed\n").unwrap();
    let msg = f.root.join("msg");
    std::fs::write(&msg, "\t\n\n  \nSigned-off-by: hula\n").unwrap();
    let (_, err, code) = f.run(&["commit", "-a", "-F", msg.to_str().unwrap()]);
    assert_eq!(code, 1, "the commit was recorded: {err}");
    assert!(
        err.contains("Aborting commit due to empty commit message."),
        "{err:?}"
    );
    assert_eq!(f.stdout(&["log", "--format=%s"]), "one\n");
}

/// `--allow-empty-message` takes it anyway, sign-offs and all.
#[test]
fn allow_empty_message_records_the_sign_offs() {
    let f = Fixture::new("allow");
    std::fs::write(f.work.join("file"), "changed\n").unwrap();
    let msg = f.root.join("msg");
    std::fs::write(&msg, "Signed-off-by: hula\n").unwrap();
    f.git(&["commit", "-a", "--allow-empty-message", "-F", msg.to_str().unwrap()]);
    assert_eq!(f.stdout(&["log", "-1", "--format=%s"]), "Signed-off-by: hula\n");
}

/// One real line among the sign-offs is a message.
#[test]
fn a_line_that_is_not_a_sign_off_is_a_message() {
    let f = Fixture::new("real");
    std::fs::write(f.work.join("file"), "changed\n").unwrap();
    let msg = f.root.join("msg");
    std::fs::write(&msg, "Signed-off-by: hula\nsubject\n").unwrap();
    f.git(&["commit", "-a", "-F", msg.to_str().unwrap()]);
    assert_eq!(f.stdout(&["log", "-1", "--format=%s"]), "Signed-off-by: hula subject\n");
}

/// `git commit --amend --only` with no path records HEAD's tree and leaves the
/// staged contents in the index.
#[test]
fn amend_only_with_no_paths_ignores_staged_contents() {
    let f = Fixture::new("amendonly");
    std::fs::write(f.work.join("file"), "staged\n").unwrap();
    f.git(&["add", "file"]);
    f.git(&["commit", "--no-edit", "--amend", "--only"]);
    assert_eq!(f.committed_file(), "committed\n", "the staged blob was committed");
    // The index still holds what was staged, so the diff against HEAD survives.
    assert_ne!(f.stdout(&["diff", "--cached", "--name-only"]), "");
}

/// `--allow-empty --only` with no path does the same for a new commit.
#[test]
fn allow_empty_only_with_no_paths_ignores_staged_contents() {
    let f = Fixture::new("emptyonly");
    std::fs::write(f.work.join("file"), "staged\n").unwrap();
    f.git(&["add", "file"]);
    f.git(&["commit", "--allow-empty", "--only", "-m", "empty"]);
    assert_eq!(f.committed_file(), "committed\n");
    assert_eq!(f.stdout(&["log", "-1", "--format=%s"]), "empty\n");
    assert_ne!(f.stdout(&["diff", "--cached", "--name-only"]), "");
}

/// Without `--amend` or `--allow-empty`, `--only` still needs a path.
#[test]
fn only_with_no_paths_is_otherwise_refused() {
    let f = Fixture::new("refused");
    let (_, err, code) = f.run(&["commit", "--only", "-m", "x"]);
    assert_eq!(code, 128, "{err:?}");
    assert!(
        err.contains("No paths with --include/--only does not make sense."),
        "{err:?}"
    );
}
