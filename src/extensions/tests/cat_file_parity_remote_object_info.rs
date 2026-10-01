//! `git cat-file --batch-command`'s `remote-object-info <remote> <oid>...`, new in
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
fn the_server_must_advertise_object_info() {
    let (srv, cl, _) = fixture("unadvertised");
    let (out, err, code) = batch(&cl, None, &format!("remote-object-info origin {BLOB}\n"));
    assert_eq!((out.as_str(), code), ("", Some(128)));
    assert_eq!(err, "fatal: object-info capability is not enabled on the server\n");
    let _ = std::fs::remove_dir_all(srv.parent().unwrap());
}

#[test]
fn malformed_requests_die_before_connecting() {
    let (srv, cl, _) = fixture("malformed");
    for (line, message) in [
        ("remote-object-info origin abcd", "remote-object-info does not support short oids, 40 characters required"),
        ("remote-object-info origin xyz", "not a valid object name 'xyz'"),
        ("remote-object-info origin", "remote-object-info requires objects"),
        ("remote-object-info ", "must supply valid remote when using remote-object-info"),
        ("remote-object-info", "remote-object-info requires arguments"),
        ("remote-object-info origin 'unclosed", "remote-object-info: failed to parse command line: unclosed quote"),
    ] {
        let (out, err, code) = batch(&cl, None, &format!("{line}\n"));
        assert_eq!((out.as_str(), code), ("", Some(128)), "{line}");
        assert_eq!(err, format!("fatal: {message}\n"), "{line}");
    }

    // Protocol v0 has no `object-info`.
    assert!(git(&srv, &["config", "transfer.advertiseObjectInfo", "true"], None).status.success());
    assert!(git(&cl, &["config", "protocol.version", "0"], None).status.success());
    let (_, err, code) = batch(&cl, None, &format!("remote-object-info origin {BLOB}\n"));
    assert_eq!(code, Some(128));
    assert_eq!(err, "fatal: object-info requires protocol v2\nfatal: the remote end hung up unexpectedly\n");

    let _ = std::fs::remove_dir_all(srv.parent().unwrap());
}
