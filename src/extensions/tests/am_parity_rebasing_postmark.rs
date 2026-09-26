//! `am --rebasing` resolves the mail's postmark with `lookup_commit_or_die()`.
//!
//! `parse_mail_rebase()` reads the `From <oid>` line and then calls
//! `lookup_commit_or_die(&commit_oid, mail)` (builtin/am.c:1469-1472). That is
//! `lookup_commit_reference()` (commit.c:81-91), which peels an annotated tag to
//! its commit and warns `<mail> <oid> is not a commit!` before replaying it; a
//! missing object dies `could not parse <mail>`, and one that peels to a
//! non-commit prints `object_as_type()`'s `error: object <oid> is a blob, not a
//! commit` first. The mail is `am_path()`, which `git_path()` spells
//! `.git/rebase-apply/0001` however deep the command runs.
//!
//! zvcs looked the id up as a commit directly: a tag postmark failed, and every
//! refusal came out as its own `could not parse commit …` text at exit 1.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

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
    /// `main` holds `base`; `side` adds one commit on top, tagged `tside`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-am-rebasing-postmark-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(work.join("sub")).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("f"), "1\n2\n3\n").unwrap();
        f.run(&["add", "f"]);
        f.run(&["commit", "-q", "-m", "base"]);
        f.run(&["checkout", "-q", "-b", "side"]);
        std::fs::write(f.work.join("f"), "1\ntwo\n3\n").unwrap();
        f.run(&["commit", "-q", "-am", "change two"]);
        f.run(&["tag", "-a", "-m", "t", "tside"]);
        f.run(&["checkout", "-q", "main"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        self.run_in(&self.work, args)
    }

    fn run_in(&self, dir: &PathBuf, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
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
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn rev(&self, spec: &str) -> String {
        self.run(&["rev-parse", spec]).0.trim().to_string()
    }

    /// `side`'s patch with its postmark rewritten to name `oid`.
    fn mailbox(&self, oid: &str) -> PathBuf {
        let (patch, _, code) = self.run(&["format-patch", "-1", "--stdout", "side"]);
        assert_eq!(code, 0);
        let (first, rest) = patch.split_once('\n').unwrap();
        assert!(first.starts_with("From "));
        let path = self.root.join("mbox");
        std::fs::write(&path, format!("From {oid}{}\n{rest}", &first[5 + oid.len()..])).unwrap();
        path
    }
}

#[test]
fn an_annotated_tag_is_peeled_with_a_warning_and_replayed() {
    let f = Fixture::new("tag");
    let tag = f.rev("tside");
    let mbox = f.mailbox(&tag);
    let (out, err, code) = f.run(&["am", "--rebasing", mbox.to_str().unwrap()]);
    let want = format!("warning: .git/rebase-apply/0001 {tag} is not a commit!\n");
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("Applying: change two\n", want.as_str(), 0)
    );
    assert_eq!(f.run(&["log", "-1", "--format=%s %an"]).0, "change two A U Thor\n");
    assert_eq!(std::fs::read_to_string(f.work.join("f")).unwrap(), "1\ntwo\n3\n");
}

#[test]
fn a_postmark_that_peels_to_a_blob_dies_after_the_type_error() {
    let f = Fixture::new("blob");
    let head = f.rev("HEAD");
    let blob = f.rev("HEAD:f");
    let mbox = f.mailbox(&blob);
    let (out, err, code) = f.run(&["am", "--rebasing", mbox.to_str().unwrap()]);
    let want = format!(
        "error: object {blob} is a blob, not a commit\n\
         fatal: could not parse .git/rebase-apply/0001\n"
    );
    assert_eq!((out.as_str(), err.as_str(), code), ("", want.as_str(), 128));
    assert_eq!(f.rev("HEAD"), head);
    assert!(f.work.join(".git/rebase-apply/0001").is_file());
}

#[test]
fn a_missing_postmark_dies_naming_the_mail_from_any_directory() {
    let f = Fixture::new("missing");
    let head = f.rev("HEAD");
    let mbox = f.mailbox("1234567890123456789012345678901234567890");
    let (out, err, code) = f.run_in(&f.work.join("sub"), &["am", "--rebasing", mbox.to_str().unwrap()]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "fatal: could not parse .git/rebase-apply/0001\n", 128)
    );
    assert_eq!(f.rev("HEAD"), head);
}
