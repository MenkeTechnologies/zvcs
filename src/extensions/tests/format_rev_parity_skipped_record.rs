//! `git format-rev --stdin-mode=revs` skipping a record (builtin/name-rev.c:898-930):
//! each failure `continue`s past the `printf("%s%c", …, output_terminator)`, so a
//! skipped record writes nothing at all to stdout, and every warning names the
//! record as typed — not a resolved object id. A full-length hex name is taken
//! without an object lookup, so a missing object fails at `parse_object()`
//! ("Could not get object for"), and a tag whose target is missing goes through
//! `deref_tag()`'s `error("missing object referenced by …")`. Expectations are
//! stock git 2.56.0's.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(repo: &Path, args: &[&str], stdin: &[u8]) -> (Vec<u8>, String, i32) {
    let mut child = Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    let out = child.wait_with_output().unwrap();
    (out.stdout, String::from_utf8_lossy(&out.stderr).into_owned(), out.status.code().unwrap_or(-1))
}

/// A one-commit repository (subject `initial`) with an annotated tag `ann`.
fn fixture(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-format-rev-skip-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["commit", "-q", "--allow-empty", "-m", "initial"],
        &["tag", "-a", "-m", "t", "ann"],
    ] {
        let (_, err, code) = git(&root, args, b"");
        assert_eq!(code, 0, "{args:?}: {err}");
    }
    root
}

#[test]
fn a_skipped_record_writes_no_terminator() {
    let repo = fixture("lf");
    let input = b"nope\nHEAD^{tree}\n1111111111111111111111111111111111111111\nann\nHEAD\n";
    let (out, err, code) = git(&repo, &["format-rev", "--format=%s", "--stdin-mode=revs"], input);
    assert_eq!(code, 0);
    assert_eq!(out, b"initial\ninitial\n");
    assert_eq!(
        err,
        "Could not get object name for nope. Skipping.\n\
         Could not get commit for HEAD^{tree}. Skipping.\n\
         Could not get object for 1111111111111111111111111111111111111111. Skipping.\n"
    );

    let (out, err, code) = git(&repo, &["format-rev", "--format=%s", "--stdin-mode=rev", "-z"], b"nope\0HEAD\0");
    assert_eq!(code, 0);
    assert_eq!(out, b"initial\0");
    assert_eq!(err, "Could not get object name for nope. Skipping.\n");
}

#[test]
fn a_tag_with_a_missing_target_is_reported_against_the_record() {
    let repo = fixture("broken-tag");
    let tag = b"object 1111111111111111111111111111111111111111\ntype commit\ntag broken\ntagger a <b> 0 +0000\n\nm\n";
    let (oid, err, code) = git(&repo, &["hash-object", "-t", "tag", "-w", "--stdin", "--literally"], tag);
    assert_eq!(code, 0, "{err}");
    let oid = String::from_utf8(oid).unwrap();
    let oid = oid.trim_end();
    assert_eq!(oid, "efe021a91321549751777019758e178f15be8245");

    let (out, err, code) = git(&repo, &["format-rev", "--format=%s", "--stdin-mode=revs"], format!("{oid}\n").as_bytes());
    assert_eq!(code, 0);
    assert_eq!(out, b"");
    assert_eq!(
        err,
        format!("error: missing object referenced by '{oid}'\nCould not get commit for {oid}. Skipping.\n")
    );
}
