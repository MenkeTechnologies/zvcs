//! `am` prints a subject the way `printf("%.*s")` does: up to the first NUL.
//!
//! `Creating an empty commit: <subject>` and its siblings take the bytes `mailinfo` produced and
//! hand them to a `%s` conversion, which stops at a NUL byte. zvcs wrote the whole slice, NUL and
//! all. Expectation measured from stock git 2.56.0.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

#[test]
fn the_subject_ends_at_a_nul() {
    let root: PathBuf = std::env::temp_dir().join(format!("zvcs-am-nul-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let env = |cmd: &mut Command| {
        cmd.current_dir(&root)
            .env("HOME", &root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("LC_ALL", "C");
    };
    let mut init = Command::new(BIN);
    env(&mut init);
    assert!(init.args(["init", "-q", "-b", "main", "."]).status().unwrap().success());
    let mut am = Command::new(BIN);
    env(&mut am);
    let mut child = am
        .args(["am", "--empty=keep"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"ab\0cd\n").unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.stdout, b"Creating an empty commit: ab\n");
    let _ = std::fs::remove_dir_all(&root);
}
