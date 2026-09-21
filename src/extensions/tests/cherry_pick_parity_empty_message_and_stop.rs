//! Two `git cherry-pick` divergences from stock 2.55.0, both measured on the
//! fixtures below under `/usr/local/bin/git` before being written here.
//!
//! * The port refused an empty commit message with
//!   `fatal: the commit message of <oid> is empty (use --allow-empty-message)`.
//!   git has no such refusal on a non-editing pick: `run_git_commit()` passes
//!   `--allow-empty-message` whenever `EDIT_MSG` is clear
//!   (`sequencer.c:1177-1178`), and `try_to_commit()`'s own check is guarded by
//!   `EDIT_MSG` (`sequencer.c:1631-1634`) while `do_commit()` only calls it with
//!   `EDIT_MSG` clear (`sequencer.c:1728`). So `cherry-pick`, `cherry-pick -x`,
//!   `cherry-pick -s` and `revert` all died at 128 on a commit written with
//!   `--allow-empty-message`. The same site appended a newline to an empty
//!   message, which `strbuf_complete_line()` does not (`sequencer.c:2394`), and
//!   skipped the implicit whitespace cleanup `-s`/`-x` get
//!   (`sequencer.c:1622-1630`) — together those put `-x`'s trailer on the third
//!   line instead of the first.
//! * The stop for a pick that became empty printed a hand-rolled header and a
//!   fixed `nothing to commit, working tree clean` tail. git runs the whole
//!   status report there (`run_status(stdout, …)`, `builtin/commit.c:1085`), so
//!   untracked files and unstaged edits to files the pick never touched — which
//!   an equal merged tree says nothing about — were simply never reported.
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
        let root = std::env::temp_dir().join(format!("zvcs-cpmsg-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.write("f.txt", "base\n");
        f.git(&["add", "f.txt"]);
        f.git(&["commit", "-q", "-m", "base"]);
        f
    }

    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.work.join(name), body).unwrap();
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@e.co")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@e.co")
            .env("GIT_AUTHOR_DATE", "2005-04-07T15:13:13-07:00")
            .env("GIT_COMMITTER_DATE", "2005-04-07T15:13:13-07:00");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = self.cmd(args).output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn stdout(&self, args: &[&str]) -> String {
        String::from_utf8_lossy(&self.cmd(args).output().unwrap().stdout).into_owned()
    }

    /// The commit object's message, byte for byte — `%B` would hide a bare
    /// newline behind `log`'s own formatting.
    fn raw_message(&self, rev: &str) -> String {
        let obj = self.stdout(&["cat-file", "commit", rev]);
        match obj.find("\n\n") {
            Some(i) => obj[i + 2..].to_string(),
            None => String::new(),
        }
    }

    /// A commit on `topic` whose message is empty and whose change is not.
    fn empty_message_commit(&self) -> String {
        self.git(&["checkout", "-q", "-b", "topic"]);
        self.write("a.txt", "a\n");
        self.git(&["add", "a.txt"]);
        self.git(&["commit", "-q", "--allow-empty-message", "-m", ""]);
        let id = self.stdout(&["rev-parse", "HEAD"]).trim().to_string();
        self.git(&["checkout", "-q", "main"]);
        id
    }
}

/// A pick that is not being edited commits an empty message verbatim — no
/// refusal, and no newline invented for it.
#[test]
fn empty_message_is_picked_without_allow_empty_message() {
    let f = Fixture::new("plain");
    f.empty_message_commit();

    let (code, out, err) = f.run(&["cherry-pick", "topic"]);
    assert_eq!(code, 0, "stdout={out:?} stderr={err:?}");
    assert!(
        !err.contains("--allow-empty-message"),
        "git never asks for that flag on a non-editing pick: {err:?}"
    );
    assert_eq!(
        f.raw_message("HEAD"),
        "",
        "an empty message must stay empty, not become a bare newline"
    );
    // The change itself still landed.
    assert_eq!(f.stdout(&["show", "--format=", "--name-only", "HEAD"]).trim(), "a.txt");
}

/// `-x` and `-s` on an empty message put their trailer on the first line: the
/// blank line they insert ahead of themselves is removed by the implicit
/// whitespace cleanup those two options select.
#[test]
fn record_origin_and_signoff_clean_an_empty_message() {
    let f = Fixture::new("trailer");
    let picked = f.empty_message_commit();

    let (code, _, err) = f.run(&["cherry-pick", "-x", "topic"]);
    assert_eq!(code, 0, "{err:?}");
    assert_eq!(
        f.raw_message("HEAD"),
        format!("(cherry picked from commit {picked})\n")
    );

    f.git(&["reset", "-q", "--hard", "HEAD~1"]);
    let (code, _, err) = f.run(&["cherry-pick", "-s", "topic"]);
    assert_eq!(code, 0, "{err:?}");
    assert_eq!(f.raw_message("HEAD"), "Signed-off-by: t <t@e.co>\n");
}

/// `git revert` of an empty-message commit works too, and names it the way git
/// does: `Revert ""`.
#[test]
fn empty_message_commit_can_be_reverted() {
    let f = Fixture::new("revert");
    let picked = f.empty_message_commit();
    f.git(&["cherry-pick", "topic"]);

    let (code, _, err) = f.run(&["revert", "--no-edit", "HEAD"]);
    assert_eq!(code, 0, "{err:?}");
    let msg = f.raw_message("HEAD");
    assert!(msg.starts_with("Revert \"\"\n\n"), "{msg:?}");
    assert!(msg.contains(&format!("This reverts commit {}", f.stdout(&["rev-parse", "HEAD~1"]).trim())), "{msg:?}");
    let _ = picked;
}

/// The stop for a pick that became empty is the full status report, so it names
/// untracked files and unstaged edits that the pick did not cause.
#[test]
fn empty_pick_stop_reports_the_real_worktree() {
    let f = Fixture::new("stop");
    f.empty_message_commit();
    f.git(&["cherry-pick", "topic"]);

    // Dirty the worktree in ways the pick has nothing to do with, then pick the
    // same commit again: its change is already in, so the pick becomes empty.
    f.write("extra.txt", "x\n");
    std::fs::create_dir_all(f.work.join("sub")).unwrap();
    f.write("sub/deep.txt", "d\n");
    f.write("f.txt", "base\nmodified\n");

    let (code, out, err) = f.run(&["cherry-pick", "topic"]);
    assert_eq!(code, 1, "the pick must stop: stdout={out:?} stderr={err:?}");

    assert!(
        out.contains("You are currently cherry-picking commit"),
        "the in-progress block still belongs to the report: {out:?}"
    );
    assert!(
        out.contains("Changes not staged for commit:") && out.contains("modified:   f.txt"),
        "an unstaged edit the pick did not make must be reported: {out:?}"
    );
    assert!(
        out.contains("Untracked files:") && out.contains("extra.txt") && out.contains("sub/"),
        "untracked paths must be reported: {out:?}"
    );
    assert!(
        !out.contains("nothing to commit, working tree clean"),
        "the clean tail is not unconditional: {out:?}"
    );
    assert!(
        out.contains("no changes added to commit"),
        "the summary line must be the one the real state earns: {out:?}"
    );

    // The advice still goes to stderr, unchanged.
    assert!(
        err.starts_with("The previous cherry-pick is now empty, possibly due to conflict resolution.\n"),
        "{err:?}"
    );
    assert!(err.ends_with("Otherwise, please use 'git cherry-pick --skip'\n"), "{err:?}");
}

/// The same stop on a genuinely clean worktree keeps the wording it had: this is
/// the half that a port passes by hard-coding the tail, so it pins the fix to
/// "report the truth" rather than "always print the long form".
#[test]
fn empty_pick_stop_on_a_clean_worktree_is_unchanged() {
    let f = Fixture::new("stopclean");
    f.empty_message_commit();
    f.git(&["cherry-pick", "topic"]);

    let (code, out, _) = f.run(&["cherry-pick", "topic"]);
    assert_eq!(code, 1);
    assert!(out.contains("On branch main\n"), "{out:?}");
    assert!(out.contains("nothing to commit, working tree clean"), "{out:?}");
    assert!(!out.contains("Untracked files:"), "{out:?}");
}
