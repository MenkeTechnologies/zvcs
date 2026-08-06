//! Which commands announce a pending EOL conversion, and how they name the path.
//!
//! git only runs the `core.safecrlf` round-trip check when it is about to write an
//! object (`get_conv_flags()` returns `global_conv_flags_eol | CONV_WRITE_OBJECT`
//! only for `HASH_WRITE_OBJECT`) or when the diff machinery converts a worktree file
//! for display (`diff_populate_filespec()` passes `global_conv_flags_eol`). The
//! content comparison behind `git status` — `ce_compare_data()` hashing with
//! `flags = 0` — is silent, and every message names the path relative to the
//! worktree root.
//!
//! Measured against stock git 2.55.0: `status` prints nothing, `add` and `diff`
//! print one warning naming `f.txt`.
//!
//! KNOWN DIVERGENCE, not asserted here: stock also warns for `diff --name-only`,
//! `--raw` and `--name-status`, where `diffcore_skip_stat_unmatch()` converts the
//! worktree file to decide whether the pair is stat-dirty only. zvcs reads content
//! for those formats without running the round-trip check, so it stays silent.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");
const WARNING: &str =
    "warning: in the working copy of 'f.txt', LF will be replaced by CRLF the next time Git touches it\n";

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
    /// A repository whose committed `f.txt` has LF endings while `core.autocrlf`
    /// asks for CRLF in the worktree, so every conversion is round-trip lossy in
    /// the direction git warns about.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-crlfwarn-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f.write("f.txt", b"a\nb\nc\n");
        f.git(&["add", "f.txt"]);
        f.git(&["commit", "-q", "-m", "init"]);
        f.git(&["config", "core.autocrlf", "true"]);
        // A second line-only edit keeps the file text and the conversion lossy.
        f.write("f.txt", b"a\nb\nc\nd\n");
        f
    }

    fn cmd(&self, dir: &PathBuf, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(&self.work, args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    fn write(&self, path: &str, body: &[u8]) {
        std::fs::write(self.work.join(path), body).unwrap();
    }

    /// stderr of `git <args>` run at `dir`.
    fn stderr_at(&self, dir: &PathBuf, args: &[&str]) -> String {
        let out = self.cmd(dir, args).output().unwrap();
        String::from_utf8_lossy(&out.stderr).into_owned()
    }

    fn stderr(&self, args: &[&str]) -> String {
        self.stderr_at(&self.work, args)
    }
}

/// The status pass hashes the worktree file to compare it with the index, which in
/// git writes no object and therefore says nothing.
#[test]
fn status_stays_silent_about_the_pending_conversion() {
    let f = Fixture::new("status");
    assert_eq!(f.stderr(&["status", "--porcelain"]), "");
    assert_eq!(f.stderr(&["status", "--short"]), "");
    assert_eq!(f.stderr(&["status"]), "");
}

/// Staging writes the blob, so the warning is exactly the one git prints.
#[test]
fn add_warns_once() {
    let f = Fixture::new("add");
    assert_eq!(f.stderr(&["add", "f.txt"]), WARNING);
}

/// The diff machinery converts the worktree side for display and warns once — the
/// index refresh that runs first must not warn a second time.
#[test]
fn diff_warns_exactly_once() {
    let f = Fixture::new("diff");
    assert_eq!(f.stderr(&["diff"]), WARNING);
    assert_eq!(f.stderr(&["diff", "HEAD"]), WARNING);
}

/// Every message names the path relative to the worktree root, whatever directory
/// the command runs in.
#[test]
fn the_warned_path_is_repo_relative_from_a_subdirectory() {
    let f = Fixture::new("subdir");
    let sub = f.work.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    assert_eq!(f.stderr_at(&sub, &["diff", "HEAD"]), WARNING);
    assert_eq!(f.stderr_at(&sub, &["status", "--porcelain"]), "");
}
