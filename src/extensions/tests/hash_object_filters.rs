//! `git hash-object` hashes what staging would store, not the raw bytes.
//!
//! `index_mem()` converts a blob whenever it has a path to look attributes up with,
//! so `hash-object --path <p> <p>` and `git add <p>` agree on the id.
//! `get_conv_flags()` then ties the `core.safecrlf` round-trip check to
//! `HASH_WRITE_OBJECT`: only `-w` warns, or refuses when the setting is `true`.
//!
//! Expectations measured against stock git 2.55.0.
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
    /// `*.txt` is `text eol=crlf`, so a LF file converts on the way in.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-hashfilter-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f.write(".gitattributes", b"*.txt text eol=crlf\n");
        f.write("lf.txt", b"a\nb\n");
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

    fn write(&self, path: &str, body: &[u8]) {
        std::fs::write(self.work.join(path), body).unwrap();
    }

    /// `(exit code, stdout trimmed, stderr)`.
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = self.cmd(args).output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

/// The blob for `a\nb\n`, which is what an unfiltered hash produces.
const RAW_ID: &str = "422c2b7ab3b3c668038da977e4e93a5fc623169c";

#[test]
fn a_path_makes_the_checkin_conversion_apply() {
    let f = Fixture::new("convert");
    let (code, id, err) = f.run(&["hash-object", "--path", "lf.txt", "lf.txt"]);
    assert_eq!(code, 0, "stderr: {err}");
    // `text eol=crlf` normalizes to LF in the object, so the id is the LF blob's —
    // the point being that it is reached through the filter, which the CRLF file below
    // proves.
    assert_eq!(id, RAW_ID);

    f.write("crlf.txt", b"a\r\nb\r\n");
    let (code, id, err) = f.run(&["hash-object", "--path", "crlf.txt", "crlf.txt"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(id, RAW_ID, "CRLF input normalizes to the same blob");

    // `--no-filters` hashes the bytes as they are, so the CRLF file differs again.
    let (_, raw, _) = f.run(&["hash-object", "--no-filters", "crlf.txt"]);
    assert_ne!(raw, RAW_ID);
}

/// The id has to be the one staging records; that is the whole point of converting.
#[test]
fn the_id_matches_what_add_stages() {
    let f = Fixture::new("agree");
    f.write("crlf.txt", b"x\r\ny\r\n");
    let (_, hashed, _) = f.run(&["hash-object", "--path", "crlf.txt", "crlf.txt"]);
    f.git(&["add", "crlf.txt"]);
    let (_, staged, _) = f.run(&["rev-parse", ":crlf.txt"]);
    assert_eq!(hashed, staged);
}

/// Writing the object is what turns the round-trip check on.
#[test]
fn only_the_writing_form_reports_a_lossy_conversion() {
    let f = Fixture::new("safecrlf");
    f.git(&["config", "core.autocrlf", "true"]);
    std::fs::remove_file(f.work.join(".gitattributes")).unwrap();

    assert_eq!(f.run(&["hash-object", "--path", "lf.txt", "lf.txt"]).2, "");
    let (code, id, err) = f.run(&["hash-object", "-w", "--path", "lf.txt", "lf.txt"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(id, RAW_ID);
    assert_eq!(
        err,
        "warning: in the working copy of 'lf.txt', LF will be replaced by CRLF the next time Git touches it\n"
    );
}

/// `core.safecrlf=true` refuses instead of warning, and only when writing.
#[test]
fn safecrlf_refuses_the_writing_form() {
    let f = Fixture::new("refuse");
    f.git(&["config", "core.autocrlf", "true"]);
    f.git(&["config", "core.safecrlf", "true"]);
    std::fs::remove_file(f.work.join(".gitattributes")).unwrap();

    let (code, _, err) = f.run(&["hash-object", "-w", "--path", "lf.txt", "lf.txt"]);
    assert_eq!(code, 128);
    assert_eq!(err, "fatal: LF would be replaced by CRLF in lf.txt\n");
    // Without `-w` there is no check to fail.
    assert_eq!(f.run(&["hash-object", "--path", "lf.txt", "lf.txt"]).0, 0);
}

/// A path that leaves the worktree has no attributes to apply.
#[test]
fn a_path_outside_the_worktree_hashes_verbatim() {
    let f = Fixture::new("outside");
    let (code, id, err) = f.run(&["hash-object", "--path", "../outside.txt", "lf.txt"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(id, RAW_ID);
}
