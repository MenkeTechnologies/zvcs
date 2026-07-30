//! An `ERR <message>` packet line is the server's own refusal.
//!
//! `upload-pack` answers a request it cannot serve with a single `ERR` line —
//! `ERR upload-pack: not our ref <oid>` for a want it cannot reach, which is
//! what a submodule fetch of a rewritten commit runs into. git's `pkt-line.c`
//! dies with `remote error: <message>` (exit 128) wherever that line appears.
//!
//! Without this, the line reaches the V2 response parser as a section header and
//! is reported as `Unknown or unsupported header: "ERR …"`, burying the server's
//! actual message in a decoding complaint.
//!
//! The fake `upload-pack` below writes its whole side up front — a v2 capability
//! advertisement, the `ls-refs` answer, then the `ERR` line — and then drains
//! stdin, so the client never sees a closed pipe instead of the error.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");
const OID: &str = "d983920472a84b255d3cb89b315c2aafa1ff55f2";

/// A packet line: four hex length bytes (counting themselves) plus the payload.
fn pkt(payload: &str) -> String {
    format!("{:04x}{payload}", payload.len() + 4)
}

struct Fixture {
    root: PathBuf,
    work: PathBuf,
    upload_pack: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-fetcherr-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let upload_pack = root.join("fake-upload-pack");

        let mut stream = String::new();
        // Capability advertisement.
        for line in ["version 2\n", "agent=fake/1\n", "ls-refs=unborn\n", "fetch=shallow\n", "object-format=sha1\n"] {
            stream.push_str(&pkt(line));
        }
        stream.push_str("0000");
        // `ls-refs` answer.
        stream.push_str(&pkt(&format!("{OID} HEAD\n")));
        stream.push_str(&pkt(&format!("{OID} refs/heads/main\n")));
        stream.push_str("0000");
        // The refusal, in place of the `fetch` answer.
        stream.push_str(&pkt(&format!("ERR upload-pack: not our ref {OID}")));

        std::fs::write(
            &upload_pack,
            format!("#!/bin/sh\nprintf '%s' '{stream}'\ncat >/dev/null\n"),
        )
        .unwrap();
        let mut perms = std::fs::metadata(&upload_pack).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&upload_pack, perms).unwrap();

        let f = Fixture { root, work, upload_pack };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        // The local transport checks the URL is a repository before it spawns
        // `upload-pack`, so the remote has to exist even though the fake server
        // never reads it.
        std::fs::create_dir_all(f.remote()).unwrap();
        let out = f
            .cmd(&["init", "-q", "-b", "main", f.remote().to_str().unwrap()])
            .output()
            .unwrap();
        assert!(out.status.success(), "remote fixture failed: {out:?}");
        f
    }

    /// The repository the fetch names as its remote.
    fn remote(&self) -> PathBuf {
        self.root.join("remote")
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }
}

/// The message the server sent is what the user sees, with git's prefix and exit
/// code — not a parser complaint about an unknown section header.
#[test]
fn err_packet_line_is_reported_as_a_remote_error() {
    let f = Fixture::new("v2");
    let out = f
        .cmd(&[
            "-c",
            "protocol.version=2",
            "fetch",
            "--upload-pack",
            f.upload_pack.to_str().unwrap(),
            f.remote().to_str().unwrap(),
            "main",
        ])
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        stderr.trim_end(),
        format!("fatal: remote error: upload-pack: not our ref {OID}"),
        "stderr: {stderr}"
    );
    assert_eq!(out.status.code(), Some(128), "wrong exit code: {stderr}");
}
