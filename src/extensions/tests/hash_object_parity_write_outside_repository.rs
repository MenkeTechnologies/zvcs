//! `git hash-object -w` outside a repository.
//!
//! ```c
//! if (flags & INDEX_WRITE_OBJECT)
//!         prefix = setup_git_directory(the_repository);
//! else
//!         prefix = setup_git_directory_gently(the_repository, &nongit);
//! ```
//!
//! (builtin/hash-object.c.) Setup runs straight after `parse_options()`, so
//! with `-w` and no repository git dies with setup's own message — before the
//! option combinations are checked, and even when nothing is named to hash.
//! Without `-w` hashing works anywhere. Expectations captured from stock git
//! 2.56.0.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "zvcs-hash-object-outside-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn git(dir: &Path, args: &[&str], stdin: &[u8]) -> Output {
    let mut child = Command::new(BIN)
        .args(args)
        .env("HOME", dir)
        .env("GIT_CEILING_DIRECTORIES", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run the binary under test");
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    child.wait_with_output().unwrap()
}

const NO_REPO: &str = "fatal: not a git repository (or any of the parent directories): .git\n";

#[test]
fn write_dies_in_setup_before_anything_else() {
    let dir = scratch("write");
    for args in [
        &["hash-object", "-w", "--stdin"][..],
        &["hash-object", "-w"][..],
        &["hash-object", "-w", "--stdin", "--stdin-paths"][..],
    ] {
        let out = git(&dir, args, b"hi\n");
        assert_eq!(out.status.code(), Some(128), "{args:?}");
        assert_eq!(String::from_utf8_lossy(&out.stderr), NO_REPO, "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
    }
}

#[test]
fn hashing_alone_needs_no_repository() {
    let dir = scratch("hash");
    let out = git(&dir, &["hash-object", "--stdin"], b"hi\n");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(out.stdout, b"45b983be36b73c0788dc9cbcb76cbb80fc7bb057\n");
}
