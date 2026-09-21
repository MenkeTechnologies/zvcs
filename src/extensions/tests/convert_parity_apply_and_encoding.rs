//! The two remaining halves of `convert_to_git()`: the pre-image `git apply` reads
//! through it, and the message a `working-tree-encoding` that cannot be decoded gets.
//!
//! `read_old_data()` (apply.c:2401-2427) runs every pre-image it reads off disk through
//! `convert_to_git()`. Without it, a patch made against normalized content misses every
//! context line of a CRLF working copy — the file on disk and the file the patch was cut
//! from differ by a `\r` per line, so `git apply` refuses a patch git applies.
//!
//! `encode_to_git()` reports a `reencode_string_len()` that answered NULL with one
//! message, `failed to encode '%s' from %s to %s` (convert.c:416-429), whether the
//! encoding was unavailable or the bytes were not valid in it — `iconv` does not
//! distinguish the two. Every expectation here is stock git 2.55.0's.
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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-convapply-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f
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

    /// `(exit code, stdout, stderr)`.
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = self.cmd(args).output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn write(&self, path: &str, body: &[u8]) {
        std::fs::write(self.work.join(path), body).unwrap();
    }

    fn read(&self, path: &str) -> Vec<u8> {
        std::fs::read(self.work.join(path)).unwrap()
    }
}

/// A patch whose lines all end `\n`, against a working copy whose lines all end `\r\n`.
///
/// `patch->crlf_in_old` stays 0 — no context or removed line ends `\r\n` — so
/// `read_old_data()` converts with `CONV_EOL_RENORMALIZE` (apply.c:2404-2405), the
/// pre-image becomes LF, every context line matches, and the result is written back
/// through `convert_to_working_tree()` as CRLF.
#[test]
fn apply_matches_an_lf_patch_against_a_crlf_working_copy() {
    let f = Fixture::new("lfpatch");
    f.write(".gitattributes", b"* text=auto\n");
    f.write("f.txt", b"l1\nl2\nl3\n");
    f.git(&["add", "-A", "."]);
    f.git(&["commit", "-qm", "one"]);
    f.git(&["config", "core.autocrlf", "true"]);
    // Materialise the working copy the way `core.autocrlf=true` wants it.
    std::fs::remove_file(f.work.join("f.txt")).unwrap();
    f.git(&["checkout-index", "-f", "f.txt"]);
    assert_eq!(f.read("f.txt"), b"l1\r\nl2\r\nl3\r\n", "the premise: a CRLF working copy");

    f.write(
        "p.patch",
        b"--- a/f.txt\n+++ b/f.txt\n@@ -1,3 +1,4 @@\n l1\n l2\n l3\n+l4\n",
    );
    let (code, _out, err) = f.run(&["apply", "p.patch"]);
    assert_eq!(code, 0, "git applies this; stderr: {err}");
    assert_eq!(err, "");
    assert_eq!(
        f.read("f.txt"),
        b"l1\r\nl2\r\nl3\r\nl4\r\n",
        "the added line is written out with the worktree's line endings too"
    );
}

/// The other arm of the same `conv_flags`. Every context line of this patch ends
/// `\r\n`, so `check_old_for_crlf()` sets `patch->crlf_in_old` (apply.c:1716-1721) and
/// `read_old_data()` passes `CONV_EOL_KEEP_CRLF`: the pre-image is left as it is, and
/// the CRLF patch matches the CRLF file.
///
/// Normalizing here instead would strip the `\r` the patch's own context lines carry
/// and the patch would no longer apply — which is the whole reason git has the flag.
#[test]
fn apply_keeps_crlf_in_the_preimage_when_the_patch_has_it() {
    let f = Fixture::new("crlfpatch");
    f.write(".gitattributes", b"* text=auto\n");
    f.write("f.txt", b"l1\nl2\nl3\n");
    f.git(&["add", "-A", "."]);
    f.git(&["commit", "-qm", "one"]);
    f.git(&["config", "core.autocrlf", "true"]);
    std::fs::remove_file(f.work.join("f.txt")).unwrap();
    f.git(&["checkout-index", "-f", "f.txt"]);

    f.write(
        "p.patch",
        b"--- a/f.txt\n+++ b/f.txt\n@@ -1,3 +1,4 @@\n l1\r\n l2\r\n l3\r\n+l4\r\n",
    );
    let (code, _out, err) = f.run(&["apply", "p.patch"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(f.read("f.txt"), b"l1\r\nl2\r\nl3\r\nl4\r\n");
}

/// A `working-tree-encoding` whose bytes the decoder will not accept. git's `iconv`
/// answers NULL and `encode_to_git()` dies with `failed to encode '%s' from %s to %s`
/// naming the encoding *as the attribute spells it* (convert.c:423-425), not as the
/// decoder normalizes it.
///
/// The bytes are SHIFT-JIS pairs that are unassigned in `iconv`'s table; a decoder that
/// accepts them has to report them through its round-trip check instead, and git's own
/// note at convert.c:445-447 records that the round-trip `die()` has no reachable case
/// precisely "because iconv errors are already caught above" — so this one message is
/// the only one a user can see.
#[test]
fn undecodable_working_tree_encoding_reports_gits_message() {
    let f = Fixture::new("shiftjis");
    f.write(".gitattributes", b"*.sj working-tree-encoding=SHIFT-JIS\n");
    f.git(&["config", "core.checkRoundtripEncoding", "SHIFT-JIS"]);
    f.write("t.sj", &[0x87, 0x90]);

    let (code, _out, err) = f.run(&["add", "t.sj"]);
    assert_eq!(code, 128, "stderr: {err}");
    assert_eq!(err, "fatal: failed to encode 't.sj' from SHIFT-JIS to UTF-8\n");

    // A second pair, to pin that the message does not depend on which byte sequence it
    // was: git says the same for every one of them.
    f.write("t.sj", &[0xfa, 0x5b]);
    let (code, _out, err) = f.run(&["add", "t.sj"]);
    assert_eq!(code, 128, "stderr: {err}");
    assert_eq!(err, "fatal: failed to encode 't.sj' from SHIFT-JIS to UTF-8\n");
}
