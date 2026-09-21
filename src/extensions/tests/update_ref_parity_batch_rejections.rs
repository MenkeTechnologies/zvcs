//! What `git update-ref --stdin --batch-updates` reports for an update it drops.
//!
//! `handle_ref_transaction_error()` keeps going for every categorised failure
//! and dies only for `REF_TRANSACTION_ERROR_GENERIC`:
//!
//! ```c
//! if (tx_err != REF_TRANSACTION_ERROR_GENERIC && opts->allow_update_failures) {
//!         print_rejected_refs(refname, old_oid, new_oid, old_target,
//!                             new_target, tx_err, err->buf, NULL);
//!         return;
//! }
//! die("%s", err->buf);
//! ```
//!
//! (builtin/update-ref.c:285-291), and `print_rejected_refs()` writes
//!
//! ```c
//! strbuf_addf(&sb, "rejected %s %s %s %s\n", refname,
//!             new_oid ? oid_to_hex(new_oid) : new_target,
//!             old_oid ? oid_to_hex(old_oid) : old_target,
//!             ref_transaction_error_msg(err));
//! ```
//!
//! (:260-263). Two things follow that the port got wrong. The last field is
//! `ref_transaction_error_msg()`'s short category (refs.c:3532-3552), not the
//! full diagnostic that already went to stderr. And a `verify` carries neither
//! `new_oid` nor `new_target` — `ref_transaction_verify()` passes NULL for both
//! — so `printf` renders the `<new-oid>` column `(null)`.
//!
//! Separately, a `create` over a reference that is already there is
//! `REF_TRANSACTION_ERROR_CREATE_EXISTS`, so the batch keeps going; the port
//! failed the whole batch for it, and the updates behind it never landed.
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
    /// One commit on `main`, plus `refs/heads/b`.
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("zvcs-update-ref-batch-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.git(&["add", "a"]);
        f.git(&["commit", "-q", "-m", "subject"]);
        f.git(&["branch", "b"]);
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

    fn oid(&self, spec: &str) -> String {
        let out = self.cmd(&["rev-parse", spec]).output().unwrap();
        assert!(out.status.success(), "rev-parse {spec}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn batch(&self, input: &str) -> (String, String, i32) {
        let mut child = self
            .cmd(&["update-ref", "--stdin", "--batch-updates"])
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

/// `create` over an existing reference drops that one update and reports it,
/// and everything behind it still applies.
#[test]
fn a_create_over_an_existing_reference_only_drops_that_update() {
    let f = Fixture::new("exists");
    let head = f.oid("HEAD");
    let zero = "0".repeat(head.len());

    assert_eq!(
        f.batch(&format!("create refs/heads/b {head}\ncreate refs/heads/ok {head}\n")),
        (
            format!("rejected refs/heads/b {head} {zero} reference already exists\n"),
            "error: cannot lock ref 'refs/heads/b': reference already exists\n".to_string(),
            0
        )
    );
    assert_eq!(f.oid("refs/heads/ok"), head, "the rest of the batch applies");
}

/// A `verify` with no old value requires the reference to be absent. It carries
/// no new value at all, so the `<new-oid>` column is `(null)` — and the batch
/// still carries on.
#[test]
fn a_verify_reports_a_null_new_value() {
    let f = Fixture::new("verify-absent");
    let head = f.oid("HEAD");
    let zero = "0".repeat(head.len());

    assert_eq!(
        f.batch(&format!("verify refs/heads/b\ncreate refs/heads/ok {head}\n")),
        (
            format!("rejected refs/heads/b (null) {zero} reference already exists\n"),
            "error: cannot lock ref 'refs/heads/b': reference already exists\n".to_string(),
            0
        )
    );
    assert_eq!(f.oid("refs/heads/ok"), head);
}

/// The last field is the category, not the diagnostic: a `verify` whose old
/// value does not match is `incorrect old value provided`, where the full
/// `is at … but expected …` goes to stderr on its own.
#[test]
fn a_mismatched_old_value_is_categorised_not_restated() {
    let f = Fixture::new("mismatch");
    let head = f.oid("HEAD");
    let wrong = "1".repeat(head.len());

    assert_eq!(
        f.batch(&format!("verify refs/heads/b {wrong}\ncreate refs/heads/ok {head}\n")),
        (
            format!("rejected refs/heads/b (null) {wrong} incorrect old value provided\n"),
            format!(
                "error: cannot lock ref 'refs/heads/b': is at {head} but expected {wrong}\n"
            ),
            0
        )
    );
    assert_eq!(f.oid("refs/heads/ok"), head);
}

/// A reference that has to exist and does not is `reference does not exist`.
#[test]
fn a_missing_reference_is_categorised() {
    let f = Fixture::new("missing");
    let head = f.oid("HEAD");
    let zero = "0".repeat(head.len());

    assert_eq!(
        f.batch(&format!("delete refs/heads/gone {head}\ncreate refs/heads/ok {head}\n")),
        (
            format!("rejected refs/heads/gone {zero} {head} reference does not exist\n"),
            "error: cannot lock ref 'refs/heads/gone': unable to resolve reference \
             'refs/heads/gone'\n"
                .to_string(),
            0
        )
    );
    assert_eq!(f.oid("refs/heads/ok"), head);
}

/// A name conflict is `refname conflict`, and the update behind it still lands.
#[test]
fn a_name_conflict_is_categorised() {
    let f = Fixture::new("conflict");
    let head = f.oid("HEAD");
    let zero = "0".repeat(head.len());

    assert_eq!(
        f.batch(&format!("create refs/heads/b/sub {head}\ncreate refs/heads/ok {head}\n")),
        (
            format!("rejected refs/heads/b/sub {head} {zero} refname conflict\n"),
            "error: cannot lock ref 'refs/heads/b/sub': 'refs/heads/b' exists; cannot create \
             'refs/heads/b/sub'\n"
                .to_string(),
            0
        )
    );
    assert_eq!(f.oid("refs/heads/ok"), head);
    assert!(!f.exists("refs/heads/b/sub"));
}
