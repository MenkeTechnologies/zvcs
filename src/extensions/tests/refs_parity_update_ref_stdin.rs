//! Two things `git update-ref --stdin` owes its caller: a name-conflict
//! diagnostic that names the reference in the way, and an answer to each
//! command before the next one is read.
//!
//! * `ref_transaction_prepare()` runs `refs_verify_refnames_available()` over
//!   the whole transaction before anything is written
//!   (refs/files-backend.c:3024-3029 → refs.c:2778-2930, v2.55.0), so a
//!   reference that would have to be both a file and a directory is refused with
//!   `'<other>' exists; cannot create '<name>'`, or
//!   `cannot process '<a>' and '<b>' at the same time` when both names are new
//!   in the same transaction. The port let the create reach the file system,
//!   where gitoxide reported only `File exists (os error 17)` — true, but it
//!   named neither reference.
//! * `report_ok()` ends in `fflush(stdout)` (builtin/update-ref.c:595-599) and
//!   the dispatch loop reads one command at a time
//!   (builtin/update-ref.c:1113-1130), which is what lets a caller write
//!   `start`, read `start: ok`, and only then decide what to send next. The port
//!   read all of stdin before answering anything, so such a caller deadlocked.
//!
//! Every expectation was measured from stock git 2.55.0 in an identical
//! throwaway repository, stdout, stderr and exit status compared separately.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
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
    /// One commit on `main`.
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("zvcs-update-ref-stdin-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.git(&["add", "a"]);
        f.git(&["commit", "-q", "-m", "subject"]);
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

    fn stdout(&self, args: &[&str]) -> String {
        let (out, err, code) = self.run(args);
        assert_eq!((err.as_str(), code), ("", 0), "`git {args:?}`");
        out
    }

    fn oid(&self, spec: &str) -> String {
        self.stdout(&["rev-parse", spec]).trim().to_string()
    }

    /// `git update-ref --stdin [<args>…]` fed `input`.
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

    fn for_each_ref(&self, prefix: &str) -> String {
        self.stdout(&["for-each-ref", "--format=%(refname)", prefix])
    }
}

/// An existing reference standing on a prefix of a new one, in both directions
/// and both stores, is named — and the transaction as a whole writes nothing.
#[test]
fn a_reference_in_the_way_of_a_create_is_named_and_the_batch_is_atomic() {
    let f = Fixture::new("prefix");
    let head = f.oid("HEAD");

    // Existing ref is a prefix of the new one.
    f.git(&["update-ref", "refs/l/c", &head]);
    let input = format!("create refs/l/b {head}\ncreate refs/l/c/x {head}\n");
    let (out, err, code) = f.stdin(&[], &input);
    assert_eq!((out.as_str(), code), ("", 128));
    assert!(
        err.contains("'refs/l/c' exists; cannot create 'refs/l/c/x'"),
        "{err:?}"
    );
    assert_eq!(f.for_each_ref("refs/l"), "refs/l/c\n", "nothing was written");

    // The other direction, out of the packed store.
    f.git(&["update-ref", "refs/p/c/x", &head]);
    f.git(&["pack-refs", "--all"]);
    let input = format!("create refs/p/c {head}\n");
    let (_, err, code) = f.stdin(&[], &input);
    assert_eq!(code, 128);
    assert!(
        err.contains("'refs/p/c/x' exists; cannot create 'refs/p/c'"),
        "{err:?}"
    );
}

/// Two names that are new in the *same* transaction get the other wording,
/// because neither exists yet for the first one to be reported as existing.
#[test]
fn two_conflicting_new_names_in_one_transaction_are_refused_together() {
    let f = Fixture::new("extras");
    let head = f.oid("HEAD");
    let input = format!(
        "create refs/n/b {head}\ncreate refs/n/c {head}\ncreate refs/n/c/x {head}\n"
    );
    let (out, err, code) = f.stdin(&[], &input);
    assert_eq!((out.as_str(), code), ("", 128));
    assert!(
        err.contains("cannot process 'refs/n/c' and 'refs/n/c/x' at the same time"),
        "{err:?}"
    );
    assert_eq!(f.for_each_ref("refs/n"), "");
}

/// A symref the update derefs through is not the name that gets written:
/// `split_symref_update()` moves the update onto the referent, so that is the
/// name the conflict is reported for — and the `create`'s must-not-exist
/// requirement follows it there too.
#[test]
fn a_conflict_behind_a_symref_is_reported_for_the_referent() {
    let f = Fixture::new("symref");
    let head = f.oid("HEAD");
    f.git(&["update-ref", "refs/s/r/foo", &head]);
    f.git(&["symbolic-ref", "refs/s/sym", "refs/s/r/foo/bar"]);

    let input = format!("create refs/s/sym {head}\ndelete refs/s/r/foo\n");
    let (_, err, code) = f.stdin(&[], &input);
    assert_eq!(code, 128);
    assert!(
        err.contains("'refs/s/r/foo' exists; cannot create 'refs/s/r/foo/bar'"),
        "{err:?}"
    );
    assert_eq!(f.for_each_ref("refs/s/r"), "refs/s/r/foo\n");
}

/// `--batch-updates` drops just the conflicting update and keeps going;
/// `ref_transaction_error_msg()` spells the reason `refname conflict`.
#[test]
fn batch_updates_rejects_only_the_conflicting_update() {
    let f = Fixture::new("batch");
    let head = f.oid("HEAD");
    f.git(&["update-ref", "refs/b/ref/foo", &head]);

    let input = format!("create refs/b/other {head}\ncreate refs/b/ref {head}\n");
    let (out, err, code) = f.stdin(&["--batch-updates"], &input);
    assert_eq!(code, 0, "{err:?}");
    let zero = "0".repeat(head.len());
    assert_eq!(out, format!("rejected refs/b/ref {head} {zero} refname conflict\n"));
    assert!(
        err.contains("'refs/b/ref/foo' exists; cannot create 'refs/b/ref'"),
        "{err:?}"
    );
    assert_eq!(
        f.for_each_ref("refs/b"),
        "refs/b/other\nrefs/b/ref/foo\n",
        "the rest of the batch still applied"
    );
}

/// The streaming contract: write one command, read its status line, and only
/// then write the next. A reader that answers only at EOF never gets here.
#[test]
fn each_transaction_command_is_answered_before_the_next_is_read() {
    let f = Fixture::new("flush");
    let head = f.oid("HEAD");
    let mut child = f
        .cmd(&["update-ref", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let output = child.stdout.take().unwrap();

    // The reader lives in its own thread so a port that never answers fails the
    // test on a timeout instead of hanging it.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(output).lines() {
            if tx.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let expect = |rx: &std::sync::mpsc::Receiver<String>, want: &str| {
        let got = rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .unwrap_or_else(|e| panic!("waiting for {want:?}: {e}"));
        assert_eq!(got, want);
    };

    writeln!(input, "start").unwrap();
    expect(&rx, "start: ok");
    writeln!(input, "create refs/heads/flush {head}").unwrap();
    writeln!(input, "prepare").unwrap();
    expect(&rx, "prepare: ok");
    writeln!(input, "commit").unwrap();
    expect(&rx, "commit: ok");
    drop(input);

    assert!(child.wait().unwrap().success());
    assert_eq!(f.oid("refs/heads/flush"), head);
}
