//! `git update-ref --stdin`: what `ref_transaction_prepare()` refuses, in git's
//! words rather than gitoxide's.
//!
//! * `ref_update_reject_duplicates()` (refs.c:2560-2582): `multiple updates for
//!   ref '<ref>' not allowed`, fatal even under `--batch-updates`.
//! * `split_head_update()` / `split_symref_update()` (refs/files-backend.c:
//!   2446-2551): an update of the branch `HEAD` names with `HEAD` also in the
//!   transaction, or of a symref whose referent is, is `multiple updates for …
//!   (including one via …) are not allowed` — a `refname conflict` rejection under
//!   `--batch-updates`, and the end of a symref cycle too.
//! * `lock_ref_for_update()`'s old-value checks (refs/files-backend.c:2680-2785,
//!   `ref_update_check_old_target()` refs.c:3149-3171): an old target against a
//!   regular ref is `expected symref with target '<old>': but is a regular ref`, a
//!   `no-deref` symref is compared by its referent (`verifying symref target: …`)
//!   and an old oid by what that referent resolves to, and a referent that does not
//!   resolve is `error reading reference`; each names the ref the command named.
//!
//! zvcs passed gitoxide's `Edit preprocessing failed …` / `is at … but expected …`
//! wording through, refused a `no-deref` old oid on a symref that git accepts, and
//! let a duplicate through `--batch-updates`.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

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
        let root = std::env::temp_dir().join(format!("zvcs-update-ref-prepare-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."], "");
        f.run(&["-c", "maintenance.auto=false", "commit", "-q", "--allow-empty", "-m", "c1"], "");
        f.run(&["branch", "other"], "");
        f.run(&["symbolic-ref", "refs/heads/s1", "refs/heads/main"], "");
        f.run(&["symbolic-ref", "refs/heads/s2", "refs/heads/s1"], "");
        f.run(&["symbolic-ref", "refs/heads/loop", "refs/heads/loop2"], "");
        f.run(&["symbolic-ref", "refs/heads/loop2", "refs/heads/loop"], "");
        f.run(&["symbolic-ref", "refs/heads/dangling", "refs/heads/gone"], "");
        f
    }

    fn run(&self, args: &[&str], stdin: &str) -> (String, String, i32) {
        let mut child = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn stdin(&self, input: &str) -> (String, String, i32) {
        self.run(&["update-ref", "--stdin"], input)
    }

    fn head(&self) -> String {
        self.run(&["rev-parse", "HEAD"], "").0.trim().to_owned()
    }
}

fn fatal(msg: &str) -> (String, String, i32) {
    (String::new(), format!("fatal: {msg}\n"), 128)
}

#[test]
fn duplicates_and_split_conflicts() {
    let f = Fixture::new("split");
    let a = f.head();
    assert_eq!(
        f.stdin(&format!("update refs/heads/d {a}\nupdate refs/heads/d {a}\n")),
        fatal("multiple updates for ref 'refs/heads/d' not allowed")
    );
    assert_eq!(
        f.run(&["update-ref", "--stdin", "--batch-updates"], &format!("update refs/heads/d {a}\nupdate refs/heads/d {a}\n")),
        fatal("multiple updates for ref 'refs/heads/d' not allowed")
    );
    assert_eq!(
        f.stdin(&format!("update refs/heads/main {a}\nupdate HEAD {a}\n")),
        fatal("multiple updates for 'HEAD' (including one via its referent 'refs/heads/main') are not allowed")
    );
    assert_eq!(
        f.stdin(&format!("update HEAD {a}\nupdate refs/heads/main {a}\n")),
        fatal("multiple updates for 'refs/heads/main' (including one via symref 'HEAD') are not allowed")
    );
    assert_eq!(
        f.stdin(&format!("update refs/heads/loop {a}\n")),
        fatal("multiple updates for 'refs/heads/loop' (including one via symref 'refs/heads/loop2') are not allowed")
    );
    // Verifying the checked-out branch is not a second update of `HEAD`.
    assert_eq!(f.stdin(&format!("verify refs/heads/main {a}\n")), (String::new(), String::new(), 0));
    // `no-deref` keeps both updates where they are.
    assert_eq!(
        f.stdin(&format!("option no-deref\nupdate refs/heads/s1 {a}\nupdate refs/heads/main {a}\n")),
        (String::new(), String::new(), 0)
    );
}

#[test]
fn old_values_are_checked_where_git_checks_them() {
    let f = Fixture::new("old");
    let a = f.head();
    assert_eq!(
        f.stdin("option no-deref\nsymref-update refs/heads/s2 refs/heads/main ref refs/heads/nope\n"),
        fatal("verifying symref target: 'refs/heads/s2': is at refs/heads/s1 but expected refs/heads/nope")
    );
    assert_eq!(
        f.stdin("option no-deref\nsymref-update refs/heads/main refs/heads/other ref refs/heads/nope\n"),
        fatal("cannot lock ref 'refs/heads/main': expected symref with target 'refs/heads/nope': but is a regular ref")
    );
    // Through the chain, still named by the ref the command gave.
    assert_eq!(
        f.stdin("symref-update refs/heads/s2 refs/heads/main ref refs/heads/other\n"),
        fatal("cannot lock ref 'refs/heads/s2': expected symref with target 'refs/heads/other': but is a regular ref")
    );
    // A missing referent is the null id: an old target still compares, an old oid
    // is missing; a cycle cannot be read at all.
    assert_eq!(
        f.stdin("option no-deref\nsymref-verify refs/heads/dangling refs/heads/gone\n"),
        (String::new(), String::new(), 0)
    );
    assert_eq!(
        f.stdin(&format!("option no-deref\nupdate refs/heads/dangling {a} {a}\n")),
        fatal(&format!("cannot lock ref 'refs/heads/dangling': reference is missing but expected {a}"))
    );
    assert_eq!(
        f.stdin("option no-deref\nsymref-verify refs/heads/loop refs/heads/loop2\n"),
        fatal("cannot lock ref 'refs/heads/loop': error reading reference")
    );
    // An old oid against a `no-deref` symref is the oid its referent resolves to.
    assert_eq!(
        f.stdin(&format!("option no-deref\nsymref-update refs/heads/s2 refs/heads/other oid {a}\n")),
        (String::new(), String::new(), 0)
    );
    assert_eq!(f.run(&["symbolic-ref", "--no-recurse", "refs/heads/s2"], "").0, "refs/heads/other\n");
}
