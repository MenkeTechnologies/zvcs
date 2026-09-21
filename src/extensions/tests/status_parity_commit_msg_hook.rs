//! Where the `commit-msg` hook sits, and what `COMMIT_EDITMSG` holds afterwards.
//!
//! ```c
//! if (!no_verify &&
//!     run_commit_hook(use_editor, index_file, NULL, "commit-msg",
//!                     git_path_commit_editmsg(), NULL))
//!         return 0;
//! ```
//!
//! (builtin/commit.c:1130-1134.) That is the last statement of
//! `prepare_to_commit()`, so the hook is handed the file exactly as the editor
//! left it — comments, status block and `-v` patch included. Only afterwards
//! does `cmd_commit()` read it back and clean it, in memory:
//! `strbuf_read_file(&sb, git_path_commit_editmsg(), 0)` then
//! `cleanup_message(&sb, cleanup_mode, verbose)` (:1902-1906), with the empty
//! message and untouched-template refusals below that (:1908-1918). Nothing ever
//! writes the cleaned text back, which is what lets the file keep serving as the
//! record of what was edited.
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
    /// A repository with one commit and a second change staged, so a plain `git
    /// commit` has both a template and something to record.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-st-cmsg-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "one\n").unwrap();
        f.git(&["add", "file"]);
        f.git(&["commit", "-q", "-m", "one"]);
        std::fs::write(f.work.join("file"), "two\n").unwrap();
        f.git(&["add", "file"]);
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

    /// Write an executable script into the repository and hand back its path.
    fn script(&self, at: &str, body: &str) -> PathBuf {
        let path = self.work.join(at);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn editmsg(&self) -> String {
        std::fs::read_to_string(self.work.join(".git/COMMIT_EDITMSG")).unwrap()
    }

    fn subject(&self) -> String {
        let out = self.cmd(&["log", "-1", "--pretty=%s"]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }
}

/// The editor prepends a subject and keeps the template, so `COMMIT_EDITMSG`
/// afterwards is that whole buffer — not the message that was recorded.
#[test]
fn commit_editmsg_keeps_what_the_editor_left() {
    let f = Fixture::new("keeps");
    let editor = f.script(
        "fake-editor",
        "mv \"$1\" \"$1.orig\"\n{ echo message; cat \"$1.orig\"; } >\"$1\"\n",
    );
    let out = f.cmd(&["commit"]).env("GIT_EDITOR", &editor).output().unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(f.subject(), "message");
    let msg = f.editmsg();
    assert!(msg.starts_with("message\n"), "COMMIT_EDITMSG was:\n{msg}");
    assert!(
        msg.contains("\n# Changes to be committed:\n"),
        "the status block was overwritten with the cleaned message:\n{msg}"
    );
}

/// The hook is handed the same buffer: the comments and the status block are
/// still there when it runs, because cleanup happens afterwards and in memory.
#[test]
fn the_hook_is_handed_the_template_not_the_cleaned_message() {
    let f = Fixture::new("raw");
    let seen = f.work.join("seen");
    f.script(
        ".git/hooks/commit-msg",
        &format!("cp \"$1\" {}\n", seen.display()),
    );
    let editor = f.script(
        "fake-editor",
        "mv \"$1\" \"$1.orig\"\n{ echo message; cat \"$1.orig\"; } >\"$1\"\n",
    );
    let out = f.cmd(&["commit"]).env("GIT_EDITOR", &editor).output().unwrap();
    assert!(out.status.success(), "{out:?}");
    let raw = std::fs::read_to_string(&seen).expect("hook never ran");
    assert!(
        raw.contains("# Please enter the commit message for your changes."),
        "hook saw a cleaned message:\n{raw}"
    );
    assert!(raw.contains("\n# Changes to be committed:\n"), "hook saw:\n{raw}");
}

/// `commit-msg` runs before the empty-message refusal, so a hook that fills an
/// emptied buffer commits rather than aborting.
#[test]
fn the_hook_runs_before_the_empty_message_refusal() {
    let f = Fixture::new("empty");
    f.script(".git/hooks/commit-msg", "echo hook-supplied >\"$1\"\n");
    let editor = f.script("fake-editor", ": >\"$1\"\n");
    let out = f.cmd(&["commit"]).env("GIT_EDITOR", &editor).output().unwrap();
    assert!(
        out.status.success(),
        "empty buffer refused before the hook could fill it: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(f.subject(), "hook-supplied");
}

/// A hook that rewrites the message wins, and its text is what gets cleaned:
/// the comment lines it leaves behind are still stripped from the object.
#[test]
fn the_hooks_buffer_is_what_gets_cleaned() {
    let f = Fixture::new("cleaned");
    f.script(
        ".git/hooks/commit-msg",
        "printf 'rewritten\\n\\n# a comment the hook left\\n' >\"$1\"\n",
    );
    let editor = f.script("fake-editor", "exit 0\n");
    let out = f.cmd(&["commit"]).env("GIT_EDITOR", &editor).output().unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(f.subject(), "rewritten");
    let body = f.cmd(&["log", "-1", "--pretty=%B"]).output().unwrap();
    let body = String::from_utf8_lossy(&body.stdout);
    assert_eq!(body.trim_end(), "rewritten", "comment survived cleanup: {body:?}");
    // The file still holds what the hook wrote, comment and all.
    assert!(f.editmsg().contains("# a comment the hook left"), "{}", f.editmsg());
}

/// A non-zero hook abandons the commit with status 1 and records nothing.
#[test]
fn a_failing_hook_abandons_the_commit() {
    let f = Fixture::new("refuse");
    f.script(".git/hooks/commit-msg", "exit 1\n");
    let editor = f.script("fake-editor", "exit 0\n");
    let out = f.cmd(&["commit"]).env("GIT_EDITOR", &editor).output().unwrap();
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(f.subject(), "one", "the refused commit was recorded anyway");
}
