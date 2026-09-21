//! Which operand `--cacheinfo` names when it dies, and what counts as the
//! new-style spelling.
//!
//! ```c
//! if (!parse_new_style_cacheinfo(ctx->argv[1], &mode, &oid, &path)) {
//!         if (add_cacheinfo(mode, &oid, path, 0))
//!                 die("git update-index: --cacheinfo cannot add %s", path);
//!         …
//! }
//! if (ctx->argc <= 3)
//!         return error("option 'cacheinfo' expects <mode>,<sha1>,<path>");
//! if (strtoul_ui(*++ctx->argv, 8, &mode) ||
//!     get_oid_hex(*++ctx->argv, &oid) ||
//!     add_cacheinfo(mode, &oid, *++ctx->argv, 0))
//!         die("git update-index: --cacheinfo cannot add %s", *ctx->argv);
//! ```
//!
//! (`cacheinfo_callback()`, builtin/update-index.c:825-837.) The `||` chain walks
//! `ctx->argv` one element per operand, so whichever operand short-circuited the
//! chain is still what `*ctx->argv` points at — the message names the mode, the
//! object or the path depending on which one was rejected, not always the path.
//!
//! `parse_new_style_cacheinfo()` (update-index.c:790-812) assigns
//! `*path = p + 1` with no emptiness test, so `<mode>,<oid>,` *is* a well-formed
//! new-style spec whose empty path `verify_path()` then rejects; treating it as
//! "not new style" would fall through to the three-argument form and report a
//! usage error under a different exit code.
//!
//! Expectations measured against stock git 2.55.0.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A blob that exists, so the only thing under test is operand parsing.
const BLOB: &str = "ce013625030ba8dba906f756967f9e9ca394464a";

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
        let root = std::env::temp_dir().join(format!("zvcs-uicache-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.ok(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("seed"), b"hello\n").unwrap();
        f.ok(&["hash-object", "-w", "seed"]);
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

    fn ok(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    /// `(exit code, stderr)`.
    fn fails(&self, args: &[&str]) -> (i32, String) {
        let out = self.cmd(args).output().unwrap();
        assert!(!out.status.success(), "`git {args:?}` unexpectedly succeeded");
        (out.status.code().unwrap(), String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

#[test]
fn legacy_form_names_the_rejected_object_not_the_path() {
    let f = Fixture::new("oid");
    let (code, err) = f.fails(&["update-index", "--add", "--cacheinfo", "100644", "zzz", "four"]);
    assert_eq!(code, 128);
    assert_eq!(err, "fatal: git update-index: --cacheinfo cannot add zzz\n");
}

#[test]
fn legacy_form_names_the_rejected_mode_not_the_path() {
    let f = Fixture::new("mode");
    let (code, err) = f.fails(&["update-index", "--add", "--cacheinfo", "9z9", BLOB, "p1"]);
    assert_eq!(code, 128);
    assert_eq!(err, "fatal: git update-index: --cacheinfo cannot add 9z9\n");
}

/// The path is only named when it is the path that failed — here `verify_path()`
/// rejects it (`add_cacheinfo()`, update-index.c:417).
#[test]
fn legacy_form_names_the_path_when_the_path_is_what_failed() {
    let f = Fixture::new("path");
    let (code, err) = f.fails(&["update-index", "--add", "--cacheinfo", "100644", BLOB, "../x"]);
    assert_eq!(code, 128);
    assert_eq!(
        err,
        "error: Invalid path '../x'\nfatal: git update-index: --cacheinfo cannot add ../x\n"
    );
}

/// An empty path is a *parsed* new-style spec, so it dies at `verify_path()` with
/// 128 rather than failing the option parser with 129.
#[test]
fn new_style_accepts_an_empty_path_and_dies_in_verify_path() {
    let f = Fixture::new("empty");
    let spec = format!("100644,{BLOB},");
    let (code, err) = f.fails(&["update-index", "--add", "--cacheinfo", &spec]);
    assert_eq!(code, 128, "usage exit 129 would mean it fell through to the legacy form");
    assert_eq!(
        err,
        "error: Invalid path ''\nfatal: git update-index: --cacheinfo cannot add \n"
    );
}

/// The three-operand form really is still reachable, and a good spec still works —
/// the operand-naming fix must not have turned every legacy call into a failure.
#[test]
fn both_spellings_still_register_an_entry() {
    let f = Fixture::new("good");
    f.ok(&["update-index", "--add", "--cacheinfo", "100644", BLOB, "legacy"]);
    let spec = format!("100644,{BLOB},modern");
    f.ok(&["update-index", "--add", "--cacheinfo", &spec]);

    let out = f.cmd(&["ls-files", "--stage"]).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("100644 {BLOB} 0\tlegacy\n100644 {BLOB} 0\tmodern\n")
    );
}
