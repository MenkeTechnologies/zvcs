//! `git unpack-objects` reports a short pack in git's words, not the decoder's.
//!
//! Every read the builtin makes goes through `fill()`:
//!
//! ```c
//! do {
//!         ssize_t ret = xread(0, buffer + len, sizeof(buffer) - len);
//!         if (ret <= 0) {
//!                 if (!ret)
//!                         die("early EOF");
//!                 die_errno("read error on input");
//!         }
//!         len += ret;
//! } while (len < min);
//! ```
//!
//! (`builtin/unpack-objects.c:78-86`.) So an empty stdin, a pack header cut
//! short, an entry cut short and a missing trailing hash are all one message:
//! `fatal: early EOF`, exit 128. The two checks immediately after the header is
//! read are the other decidable fatals — `bad pack file` for a wrong signature
//! and `unknown pack file version <n>` for a version git will not take
//! (`builtin/unpack-objects.c:587-592`).
//!
//! The port reaches all of these through `gix-pack`, whose diagnostics are its
//! own ("An IO operation failed while streaming an entry", "Pack data type not
//! recognized"). Those leaked straight to stderr; a caller matching on git's
//! text saw none of the three. The error chain says which fatal it is —
//! `read_exact` gives `UnexpectedEof`, the header decoder gives its own error
//! type — so the translation is a fact about the chain rather than a guess.
//!
//! Every expectation was captured from stock git 2.55.0. `-n` keeps each case a
//! pure decode with nothing written.
#![cfg(unix)]

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

const EARLY_EOF: &str = "fatal: early EOF\n";

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
        let root = std::env::temp_dir().join(format!("zvcs-unpacktrunc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let fx = Fixture { root, work };
        fx.ok(&["init", "-q", "-b", "main", "."]);
        fx
    }

    fn feed(&self, args: &[&str], stdin: &[u8]) -> Output {
        let mut child = Command::new(BIN)
            .args(["-c", "user.email=t@e.co", "-c", "user.name=t"])
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("ZVCS_HOME", &self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.root.join("gitconfig"))
            .env("GIT_CONFIG_SYSTEM", self.root.join("gitconfig-system"))
            .env("LC_ALL", "C")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("run binary");
        child.stdin.take().unwrap().write_all(stdin).unwrap();
        child.wait_with_output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> Output {
        let out = self.feed(args, b"");
        assert!(out.status.success(), "setup git {args:?}: {out:?}");
        out
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A 12-byte pack header: `PACK`, big-endian version, big-endian object count.
fn header(version: u32, objects: u32) -> Vec<u8> {
    let mut v = b"PACK".to_vec();
    v.extend_from_slice(&version.to_be_bytes());
    v.extend_from_slice(&objects.to_be_bytes());
    v
}

#[test]
fn every_short_read_is_early_eof() {
    let fx = Fixture::new("eof");

    for (label, input) in [
        // Nothing at all: the very first `fill(sizeof(struct pack_header))`.
        ("empty", Vec::new()),
        // Eight bytes where twelve were asked for.
        ("short header", b"PACK0000".to_vec()),
        // A complete header promising an object that never arrives.
        ("no entry", header(2, 1)),
        // An entry that starts and stops.
        ("partial entry", [header(2, 1), vec![0xff, 0xff, 0xff]].concat()),
    ] {
        let out = fx.feed(&["unpack-objects", "-n"], &input);
        assert_eq!(out.status.code(), Some(128), "{label} exit: {out:?}");
        assert_eq!(stderr(&out), EARLY_EOF, "{label} stderr");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "", "{label} stdout");
    }
}

#[test]
fn a_wrong_signature_is_bad_pack_file() {
    let fx = Fixture::new("sig");

    // `if (get_be32(hdr) != PACK_SIGNATURE) die("bad pack file")` — the check
    // runs on a *complete* header, so twelve bytes have to arrive first or the
    // answer would be `early EOF` instead.
    let mut input = header(2, 1);
    input[..4].copy_from_slice(b"XXXX");
    let out = fx.feed(&["unpack-objects", "-n"], &input);
    assert_eq!(out.status.code(), Some(128), "{out:?}");
    assert_eq!(stderr(&out), "fatal: bad pack file\n");
}

#[test]
fn an_unusable_version_names_itself() {
    let fx = Fixture::new("version");

    // `die("unknown pack file version %"PRIu32, get_be32(hdr))`. Versions 2 and
    // 3 are the ones git takes, so 0, 1 and 4 all land here with their own
    // number in the message.
    for version in [0u32, 1, 4] {
        let out = fx.feed(&["unpack-objects", "-n"], &header(version, 1));
        assert_eq!(out.status.code(), Some(128), "version {version} exit: {out:?}");
        assert_eq!(
            stderr(&out),
            format!("fatal: unknown pack file version {version}\n"),
            "version {version} stderr"
        );
    }
}

#[test]
fn a_well_formed_pack_still_unpacks() {
    let fx = Fixture::new("good");

    std::fs::write(fx.work.join("f.txt"), "hello\n").unwrap();
    fx.ok(&["add", "f.txt"]);
    fx.ok(&["commit", "-q", "-m", "one"]);
    let head = String::from_utf8_lossy(&fx.ok(&["rev-parse", "HEAD"]).stdout).trim_end().to_string();
    let pack = fx.feed(&["pack-objects", "--revs", "--stdout"], format!("{head}\n").as_bytes());
    assert!(pack.status.success(), "pack-objects: {pack:?}");
    assert!(pack.stdout.starts_with(b"PACK"), "pack-objects produced no pack: {pack:?}");

    // Into a second, empty repository — the translation must only fire on real
    // failures, and a valid stream must stay silent and exit 0.
    let other = Fixture::new("good-dst");
    let out = other.feed(&["unpack-objects"], &pack.stdout);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(stderr(&out), "", "{out:?}");

    let typed = other.feed(&["cat-file", "-t", &head], b"");
    assert_eq!(String::from_utf8_lossy(&typed.stdout), "commit\n", "{typed:?}");
}
