//! What `git update-ref` says when a reference cannot be written, and which of
//! git's two wordings it uses.
//!
//! A name conflict is found in one of two places, and the two read differently:
//!
//! * `lock_raw_ref()` meets it while taking the lock — a *file* where a leading
//!   directory has to go (refs/files-backend.c:743-753), or a non-empty
//!   directory where the reference has to go (:864-884) — and its caller wraps
//!   whatever `refs_verify_refname_available()` said:
//!
//!   ```c
//!   reason = strbuf_detach(err, NULL);
//!   strbuf_addf(err, "cannot lock ref '%s': %s",
//!               ref_update_original_update_refname(update), reason);
//!   ```
//!
//!   (refs/files-backend.c:2667-2674).
//! * `files_transaction_prepare()` defers the rest to one batched check after
//!   the locks are held (:3024-3029), and reports it bare.
//!
//! So the same collision is `cannot lock ref 'X': 'P' exists; cannot create 'X'`
//! while `P` is a loose file, and just `'P' exists; cannot create 'X'` once
//! `pack-refs --all` has moved it into `packed-refs`.
//!
//! The single-reference command line runs the same transaction, so it owes the
//! same diagnostics; the port ran none of these checks there and let gitoxide
//! reach the file system, where the answers were `File exists (os error 17)` and
//! a reflog that `Is a directory` — naming neither reference.
//!
//! Every expectation was measured from stock git 2.55.0 in an identical
//! throwaway repository, stdout, stderr and exit status compared separately.
#![cfg(unix)]

use std::io::Write;
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
    /// One commit on `main`, plus `refs/heads/b` and `refs/heads/feature/x`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-update-ref-conflict-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.git(&["add", "a"]);
        f.git(&["commit", "-q", "-m", "subject"]);
        f.git(&["branch", "b"]);
        f.git(&["branch", "feature/x"]);
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

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = self.cmd(args).output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn oid(&self, spec: &str) -> String {
        let out = self.cmd(&["rev-parse", spec]).output().unwrap();
        assert!(out.status.success(), "rev-parse {spec}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn stdin(&self, args: &[&str], input: &str) -> (String, String, i32) {
        let mut argv = vec!["update-ref", "--stdin"];
        argv.extend_from_slice(args);
        let mut child = self
            .cmd(&argv)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn exists(&self, refname: &str) -> bool {
        self.cmd(&["rev-parse", "--verify", "-q", refname])
            .output()
            .unwrap()
            .status
            .success()
    }
}

/// The single-reference form runs the availability check and names the
/// reference in the way, in both directions, with the lock wrapping.
#[test]
fn the_command_line_form_names_the_reference_in_the_way() {
    let f = Fixture::new("cmdline");
    let head = f.oid("HEAD");

    // A loose `refs/heads/b` is a file where `refs/heads/b/sub` needs a directory.
    assert_eq!(
        f.run(&["update-ref", "refs/heads/b/sub", &head]),
        (
            String::new(),
            "fatal: update_ref failed for ref 'refs/heads/b/sub': cannot lock ref \
             'refs/heads/b/sub': 'refs/heads/b' exists; cannot create 'refs/heads/b/sub'\n"
                .to_string(),
            128
        )
    );
    // And the other way round: `refs/heads/feature` is a directory holding `x`.
    assert_eq!(
        f.run(&["update-ref", "refs/heads/feature", &head]),
        (
            String::new(),
            "fatal: update_ref failed for ref 'refs/heads/feature': cannot lock ref \
             'refs/heads/feature': 'refs/heads/feature/x' exists; cannot create \
             'refs/heads/feature'\n"
                .to_string(),
            128
        )
    );
    assert!(!f.exists("refs/heads/b/sub"));
}

/// Once the blocking reference is packed there is no file and no directory in
/// the way, so the lock succeeds and the deferred check reports it bare.
#[test]
fn a_packed_blocker_is_reported_without_the_lock_wrapping() {
    let f = Fixture::new("packed");
    let head = f.oid("HEAD");
    f.git(&["pack-refs", "--all"]);

    assert_eq!(
        f.run(&["update-ref", "refs/heads/b/sub", &head]),
        (
            String::new(),
            "fatal: update_ref failed for ref 'refs/heads/b/sub': 'refs/heads/b' exists; \
             cannot create 'refs/heads/b/sub'\n"
                .to_string(),
            128
        )
    );
    assert_eq!(
        f.stdin(&[], &format!("create refs/heads/b/sub {head}\n")),
        (
            String::new(),
            "fatal: 'refs/heads/b' exists; cannot create 'refs/heads/b/sub'\n".to_string(),
            128
        )
    );
}

/// Two names of the same transaction collide, and git always reports the pair
/// from the *shorter* name: it is the one `refnames_to_check` reaches first, and
/// the one whose lock the longer name's directory blocks.
///
/// Which of the two wordings appears depends on the order the commands arrived
/// in, because the locks are taken in that order: a child created first has
/// already made the directory by the time its parent is locked.
#[test]
fn a_same_transaction_collision_is_reported_from_the_shorter_name() {
    let f = Fixture::new("extras");
    let head = f.oid("HEAD");

    assert_eq!(
        f.stdin(&[], &format!("create refs/heads/q {head}\ncreate refs/heads/q/r {head}\n")),
        (
            String::new(),
            "fatal: cannot process 'refs/heads/q' and 'refs/heads/q/r' at the same time\n"
                .to_string(),
            128
        )
    );
    assert_eq!(
        f.stdin(&[], &format!("create refs/heads/z/r {head}\ncreate refs/heads/z {head}\n")),
        (
            String::new(),
            "fatal: cannot lock ref 'refs/heads/z': cannot process 'refs/heads/z' and \
             'refs/heads/z/r' at the same time\n"
                .to_string(),
            128
        )
    );
    assert!(!f.exists("refs/heads/q"));
    assert!(!f.exists("refs/heads/z/r"));
}

/// A stale `<ref>.lock` is `unable_to_lock_message()`'s `EEXIST` branch
/// (lockfile.c:250-291): the *absolute* path of the lock file, `File exists.`,
/// a blank line, and the holder paragraph. gitoxide reports its own attempt
/// count and timeout instead, which says nothing about who holds the lock.
#[test]
fn a_stale_lock_file_is_reported_in_gits_two_paragraphs() {
    let f = Fixture::new("stale");
    let head = f.oid("HEAD");
    let lock = f.work.join(".git/refs/heads/b.lock");
    std::fs::write(&lock, "").unwrap();

    let (out, err, code) = f.run(&["update-ref", "refs/heads/b", &head]);
    assert_eq!((out.as_str(), code), ("", 128));
    assert_eq!(
        err,
        format!(
            "fatal: update_ref failed for ref 'refs/heads/b': cannot lock ref 'refs/heads/b': \
             Unable to create '{}': File exists.\n\nAnother git process seems to be running in \
             this repository, or the lock file may be stale\n",
            lock.canonicalize().unwrap().display()
        )
    );
}

/// `lock_raw_ref()` answers `mustexist` over a missing reference with one
/// message for both of its arms (refs/files-backend.c:840-844, :873-878), and
/// the caller wraps it. gitoxide splits a deletion out into its own error, whose
/// own text ("did not exist or could not be parsed") is not git's.
#[test]
fn deleting_a_missing_reference_with_an_old_value_says_unable_to_resolve() {
    let f = Fixture::new("delete");
    let head = f.oid("HEAD");

    assert_eq!(
        f.run(&["update-ref", "-d", "refs/heads/gone", &head]),
        (
            String::new(),
            "error: cannot lock ref 'refs/heads/gone': unable to resolve reference \
             'refs/heads/gone'\n"
                .to_string(),
            1
        )
    );
    assert_eq!(
        f.stdin(&[], &format!("delete refs/heads/gone {head}\n")),
        (
            String::new(),
            "fatal: cannot lock ref 'refs/heads/gone': unable to resolve reference \
             'refs/heads/gone'\n"
                .to_string(),
            128
        )
    );
}
