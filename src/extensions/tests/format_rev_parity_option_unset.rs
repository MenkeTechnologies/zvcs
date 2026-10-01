//! `git format-rev`'s option table (builtin/name-rev.c:828-843) as
//! `parse_options()` reads it: `--format` and `--stdin-mode` are plain
//! `OPT_STRING`s, so their `--no-` spelling resets them to NULL and the
//! "is required" check fires; and positionals are collected while the sweep
//! goes on, so `too many arguments` (name-rev.c:847-850) is reported only
//! after every option has been parsed. Expectations are stock git 2.56.0's.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A fresh repository under a directory unique to this test.
fn fixture(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-format-rev-unset-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let status = Command::new(BIN)
        .args(["init", "-q", "-b", "main"])
        .current_dir(&root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap();
    assert!(status.success());
    root
}

/// `git format-rev <args>` with `HEAD` on stdin: (stdout, stderr, status).
fn format_rev(repo: &PathBuf, args: &[&str]) -> (String, String, i32) {
    let mut child = Command::new(BIN)
        .arg("format-rev")
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"HEAD\n").unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

const USAGE_LINE: &str = "usage: (EXPERIMENTAL!) git format-rev --stdin-mode=<mode> --format=<pretty> \
[--[no-]notes=<ref>] [-z] [--[no-]null-output] [--[no-]null-input]\n";

#[test]
fn no_format_and_no_stdin_mode_unset_the_value() {
    let repo = fixture("unset");
    let (out, err, code) = format_rev(
        &repo,
        &["--format=%s", "--format=oneline", "--notes=refs/notes/commits", "--null-output", "--no-format"],
    );
    assert_eq!((out.as_str(), err.as_str(), code), ("", "fatal: '--format' is required\n", 128));

    let (out, err, code) = format_rev(&repo, &["--format=%s", "--stdin-mode=rev", "--no-stdin-mode"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "fatal: '--stdin-mode' is required\n", 128));

    let (_, err, code) = format_rev(&repo, &["--no-format=x"]);
    assert_eq!((err.as_str(), code), ("error: option `no-format' takes no value\n", 129));
}

#[test]
fn positionals_are_refused_after_the_sweep() {
    let repo = fixture("positional");
    let (_, err, code) = format_rev(&repo, &["--format=%H", "--stdin-mode=rev", "foo", "--bogus"]);
    assert_eq!(code, 129);
    assert!(err.starts_with(&format!("error: unknown option `bogus'\n{USAGE_LINE}")), "{err}");

    for args in [&["--format=%H", "--stdin-mode=rev", "--", "foo"][..], &["-", "--format=%H"]] {
        let (_, err, code) = format_rev(&repo, args);
        assert_eq!(code, 129, "{args:?}");
        assert!(err.starts_with(&format!("error: too many arguments\n{USAGE_LINE}")), "{args:?}: {err}");
    }
}
