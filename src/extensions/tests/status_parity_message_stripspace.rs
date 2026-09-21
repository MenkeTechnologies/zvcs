//! The whitespace pass `prepare_to_commit()` runs over the message buffer
//! before anything else sees it.
//!
//! ```c
//! if (clean_message_contents)
//!         strbuf_stripspace(&sb, NULL);
//! if (signoff)
//!         append_signoff(&sb, ignored_log_message_bytes(sb.buf, sb.len), 0);
//! if (fwrite(sb.buf, 1, sb.len, s->fp) < sb.len)
//!         die_errno(_("could not write commit template"));
//! ```
//!
//! (builtin/commit.c:924-931.) `clean_message_contents` is `cleanup_mode !=
//! COMMIT_MSG_CLEANUP_NONE` (:773), and the one seed that turns it back off is a
//! `-t <file>` template (:887), which must reach the editor exactly as written.
//! The `NULL` comment string means whitespace only: no comment line is removed
//! here.
//!
//! Because it runs before the write, what it produces is what the editor opens
//! on, what the hooks read, and what `template_untouched()` compares.
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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-st-strip-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "one\n").unwrap();
        f.git(&["add", "file"]);
        f.git(&["commit", "-q", "-m", "first"]);
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

    fn write(&self, name: &str, body: &str) -> String {
        let path = self.root.join(name);
        std::fs::write(&path, body).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn editmsg(&self) -> String {
        std::fs::read_to_string(self.work.join(".git/COMMIT_EDITMSG")).unwrap()
    }
}

/// `git commit -e -F <file>`: the buffer the editor opens on has the file's
/// leading and trailing blank lines gone, so the status block starts on the
/// line git puts it on.
#[test]
fn an_edited_f_message_reaches_the_editor_stripped() {
    let f = Fixture::new("edit");
    let msg = f.write("text", "\nsample\n\n");
    f.git(&["commit", "-e", "-F", &msg, "-a"]);
    let editmsg = f.editmsg();
    let lines: Vec<&str> = editmsg.lines().take(4).collect();
    assert_eq!(
        lines,
        vec![
            "sample",
            "",
            "# Please enter the commit message for your changes. Lines starting",
            "# with '#' will be ignored, and an empty message aborts the commit.",
        ],
        "COMMIT_EDITMSG was:\n{editmsg}"
    );
}

/// Runs of blank lines inside the message collapse to one, and comment lines
/// survive: the pass is given a `NULL` comment string.
#[test]
fn blank_runs_collapse_and_comments_survive() {
    let f = Fixture::new("runs");
    let msg = f.write("text", "subject\n\n\n\nbody\n# not a comment to strip\n");
    f.git(&["commit", "-e", "-F", &msg, "-a"]);
    let body = f.editmsg();
    let (kept, _) = body.split_once("\n# Please").unwrap_or((body.as_str(), ""));
    assert!(
        kept.starts_with("subject\n\nbody\n# not a comment to strip\n"),
        "buffer was:\n{kept}"
    );
}

/// `--cleanup=verbatim` is `COMMIT_MSG_CLEANUP_NONE`, which turns the pass off.
#[test]
fn verbatim_cleanup_keeps_the_buffer_as_written() {
    let f = Fixture::new("verbatim");
    let msg = f.write("text", "\nsample\n\n");
    f.git(&["commit", "-e", "--cleanup=verbatim", "-F", &msg, "-a"]);
    assert!(
        f.editmsg().starts_with("\nsample\n\n"),
        "COMMIT_EDITMSG was:\n{}",
        f.editmsg()
    );
}

/// A `-t <file>` template is the seed git exempts, so it reaches the editor
/// byte for byte.
#[test]
fn a_template_file_reaches_the_editor_unstripped() {
    let f = Fixture::new("template");
    let tmpl = f.write("tmpl", "\ntemplate line\n\n");
    // The template is untouched, so the commit is refused -- which is the state
    // that proves the editor was handed it verbatim.
    let out = f.cmd(&["commit", "-t", &tmpl]).output().unwrap();
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(
        f.editmsg().starts_with("\ntemplate line\n\n"),
        "COMMIT_EDITMSG was:\n{}",
        f.editmsg()
    );
}

/// The sign-off is appended after the pass, so it lands under the stripped
/// message rather than under the blank lines it removed.
#[test]
fn the_sign_off_follows_the_stripped_message() {
    let f = Fixture::new("signoff");
    let msg = f.write("text", "subject\n\n\n");
    f.git(&["commit", "-s", "-F", &msg, "-a"]);
    let out = f.cmd(&["log", "-1", "--format=%B"]).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "subject\n\nSigned-off-by: C O Mitter <committer@example.com>\n\n"
    );
}
