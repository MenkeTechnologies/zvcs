//! The answers half of `git cat-file --batch-command`'s `remote-object-info <remote> <oid>...`, new in
//! git 2.56, and the protocol-v2 `object-info` command it talks to.
//!
//! Client (builtin/cat-file.c:683-886, fetch-object-info.c:53-185): the line is
//! split like a shell command line, every word after the remote must be a full
//! object id, and the server is asked over protocol v2 for the attributes it
//! advertises among those the format names. Only `%(objectname)` and the
//! attributes that came back expand; every other atom expands to nothing. An id
//! the server does not know is `<oid> missing`.
//!
//! Server (serve.c:95-107, protocol-caps.c:64-155): `transfer.advertiseObjectInfo`
//! advertises `object-info=size type`; each answer carries the requested
//! attributes, and an unknown id is the id plus one space.
//!
//! Both ends here are this binary, over `file://`. Expectations measured from
//! stock git 2.56.0 on the same fixture.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// The blob `hello\n` and the commit holding it, under the fixed dates below.
const BLOB: &str = "ce013625030ba8dba906f756967f9e9ca394464a";
const MISSING: &str = "0123456789012345678901234567890123456789";

fn git(dir: &Path, args: &[&str], stdin: Option<&str>) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@e")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@e")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    {
        use std::io::Write;
        let mut input = child.stdin.take().unwrap();
        if let Some(text) = stdin {
            input.write_all(text.as_bytes()).unwrap();
        }
    }
    child.wait_with_output().unwrap()
}

/// `srv` with one commit, and an empty `cl` whose `origin` is `file://…/srv`.
fn fixture(tag: &str) -> (PathBuf, PathBuf, String) {
    let root = std::env::temp_dir().join(format!("zvcs-remote-object-info-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    assert!(git(&root, &["init", "-q", "-b", "main", "srv"], None).status.success());
    let srv = root.join("srv");
    std::fs::write(srv.join("a"), "hello\n").unwrap();
    assert!(git(&srv, &["add", "a"], None).status.success());
    assert!(git(&srv, &["commit", "-q", "-m", "i"], None).status.success());
    let commit = String::from_utf8(git(&srv, &["rev-parse", "HEAD"], None).stdout).unwrap().trim().to_owned();
    assert!(git(&root, &["init", "-q", "cl"], None).status.success());
    let cl = root.join("cl");
    let url = format!("file://{}", srv.display());
    assert!(git(&cl, &["remote", "add", "origin", &url], None).status.success());
    (srv, cl, commit)
}

fn batch(cl: &Path, format: Option<&str>, input: &str) -> (String, String, Option<i32>) {
    let arg = match format {
        Some(f) => format!("--batch-command={f}"),
        None => "--batch-command".to_owned(),
    };
    let out = git(cl, &["cat-file", &arg], Some(input));
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
    )
}

#[test]
fn sizes_and_types_come_from_the_server() {
    let (srv, cl, commit) = fixture("answers");
    assert!(git(&srv, &["config", "transfer.advertiseObjectInfo", "true"], None).status.success());

    let (out, err, code) = batch(&cl, None, &format!("remote-object-info origin {BLOB} {commit} {MISSING}\n"));
    assert_eq!((err.as_str(), code), ("", Some(0)));
    assert_eq!(out, format!("{BLOB} blob 6\n{commit} commit 116\n{MISSING} missing\n"));

    // Atoms the server cannot fill expand to nothing.
    let (out, _, code) = batch(
        &cl,
        Some("%(objectname) %(objecttype) %(objectsize) [%(objectsize:disk)] [%(rest)] [%(objectmode)] [%(deltabase)]"),
        &format!("remote-object-info origin {BLOB} {MISSING}\n"),
    );
    assert_eq!(code, Some(0));
    assert_eq!(out, format!("{BLOB} blob 6 [] [] [] []\n{MISSING} missing\n"));

    // A word longer than an id is read for its first 40 digits; the remote can be a URL.
    let url = format!("file://{}", srv.display());
    let (out, _, code) = batch(&cl, Some("%(objectname)|%(objectsize)"), &format!("remote-object-info {url} {BLOB}ff\n"));
    assert_eq!((out, code), (format!("{BLOB}|6\n"), Some(0)));

    // Local lookups are unaffected around it: `cl` has none of these objects.
    let (out, _, code) = batch(&cl, None, &format!("info {BLOB}\nremote-object-info origin {BLOB}\ninfo HEAD\n"));
    assert_eq!((out, code), (format!("{BLOB} missing\n{BLOB} blob 6\nHEAD missing\n"), Some(0)));

    let _ = std::fs::remove_dir_all(srv.parent().unwrap());
}
