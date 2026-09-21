//! A branch that is itself a symbolic ref must still be checkout-able.
//!
//! `git symbolic-ref refs/heads/a-branch refs/remotes/origin/HEAD` is a legal
//! thing to write, and t2018-checkout-branch.sh builds exactly that before
//! running `git checkout -f a-branch` twice. git reads such a ref through
//! `resolve_ref_unsafe()` (refs.c), which chases up to `SYMREF_MAXDEPTH` links
//! and returns the object at the end of the chain — here `refs/remotes/origin/
//! HEAD` → `refs/remotes/origin/main` → a commit.
//!
//! The port took the reference's own target instead, which for a symbolic one
//! is not an object id, and aborted the process:
//!
//! ```text
//! thread 'main' panicked at src/ported/gix/src/reference/mod.rs:34:14:
//! BUG: tries to obtain object id from symbolic target
//! ```
//!
//! A panic is not a diagnostic — the shell sees a signal-free 101 and a Rust
//! backtrace where git prints nothing and switches branches. `switch` and
//! `restore --source` read the same helper, so all three were affected.
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
    /// One commit on `main`, plus `refs/heads/sym`, a symbolic ref pointing at
    /// `refs/heads/main` — the shape t2018 builds via a clone's
    /// `refs/remotes/origin/HEAD`, without needing the clone.
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("zvcs-idx-symref-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "one\n").unwrap();
        f.git(&["add", "a"]);
        f.git(&["commit", "-q", "-m", "one"]);
        f.git(&["symbolic-ref", "refs/heads/sym", "refs/heads/main"]);
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
        let err = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(
            !err.contains("panicked at"),
            "`git {args:?}` panicked: {err}"
        );
        (err, out.status.code().expect("no signal"))
    }

    fn head_oid(&self) -> String {
        let out = self.cmd(&["rev-parse", "HEAD"]).output().unwrap();
        assert!(out.status.success());
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }
}

/// The regression: `checkout -f` on a symbolic branch resolves and succeeds,
/// twice — the second run is the no-op switch t2018 also exercises.
#[test]
fn index_parity_checkout_follows_a_symbolic_branch_ref() {
    let f = Fixture::new("checkout");
    let want = f.head_oid();

    let (err, code) = f.run(&["checkout", "-f", "sym"]);
    assert_eq!(code, 0, "checkout of a symbolic branch failed: {err}");
    assert_eq!(f.head_oid(), want);

    let (err, code) = f.run(&["checkout", "-f", "sym"]);
    assert_eq!(code, 0, "re-checkout of a symbolic branch failed: {err}");
    assert_eq!(f.head_oid(), want);
}

/// `switch` and `restore --source` read the same helper, so neither may panic
/// on the same ref.
#[test]
fn index_parity_switch_and_restore_follow_a_symbolic_branch_ref() {
    let f = Fixture::new("switch");
    let want = f.head_oid();

    let (err, code) = f.run(&["switch", "--detach", "sym"]);
    assert_eq!(code, 0, "switch --detach of a symbolic branch failed: {err}");
    assert_eq!(f.head_oid(), want);

    std::fs::write(f.work.join("a"), "dirty\n").unwrap();
    let (err, code) = f.run(&["restore", "--source", "sym", "--", "a"]);
    assert_eq!(code, 0, "restore --source from a symbolic branch failed: {err}");
    assert_eq!(std::fs::read_to_string(f.work.join("a")).unwrap(), "one\n");
}
