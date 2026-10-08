//! A patch whose last line is cut off before its newline is corrupt at that line.
//!
//! `parse_fragment()` rejects a body line with `if (!len || line[len-1] != '\n') return -1;`
//! (apply.c), before it looks at the line's first byte, and `state->linenr` still names the
//! line that failed. That holds with `--recount` too: the option only stops the loop trusting
//! the header's counts, it does not make an unterminated line a line. zvcs counted the cut-off
//! line as a body line, so without `--recount` it reported the line *after* it
//! (`<stdin>:6` where git says `<stdin>:5`) and with `--recount` it went on to look for the
//! hunk in the file.
//!
//! Expectations come from stock git (`support/stock_git.rs`) in an identical repository.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn git(bin: &str, dir: &Path, stdin: &[u8], args: &[&str]) -> (String, String, i32) {
    let mut child = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir.parent().unwrap())
        .env("GIT_CEILING_DIRECTORIES", dir.parent().unwrap())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

fn repo(label: &str, bin: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-apply-trunc-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let work = std::fs::canonicalize(&work).unwrap();
    git(bin, &work, b"", &["init", "-q", "-b", "main", "."]);
    std::fs::write(work.join("README.md"), "# fixture\n").unwrap();
    work
}

fn same(label: &str, patch: &[u8], args: &[&str]) {
    let Some(stock) = stock_git::stock_git() else { return };
    let s = repo(&format!("{label}-stock"), stock);
    let z = repo(&format!("{label}-zvcs"), ZVCS);
    let want = git(stock, &s, patch, args);
    let got = git(ZVCS, &z, patch, args);
    let _ = std::fs::remove_dir_all(s.parent().unwrap());
    let _ = std::fs::remove_dir_all(z.parent().unwrap());
    assert_eq!(got, want, "git {args:?}: left is zvcs, right is stock");
    assert!(want.1.contains("corrupt patch at <stdin>:"), "the oracle no longer calls it corrupt: {want:?}");
}

const CUT_CONTEXT: &[u8] =
    b"diff --git a/README.md b/README.md\n--- a/README.md\n+++ b/README.md\n@@ -1 +1,2 @@\n # fixt";
const CUT_ADDED: &[u8] =
    b"diff --git a/README.md b/README.md\n--- a/README.md\n+++ b/README.md\n@@ -1 +1,2 @@\n # fixture\n+add";

#[test]
fn counts_not_yet_satisfied_names_the_cut_off_line() {
    same("ctx", CUT_CONTEXT, &["apply"]);
    same("ctx-check", CUT_CONTEXT, &["apply", "--check"]);
    same("added", CUT_ADDED, &["apply", "--stat"]);
}

#[test]
fn recount_does_not_make_the_cut_off_line_a_body_line() {
    same("ctx-recount", CUT_CONTEXT, &["apply", "--recount"]);
    same("ctx-recount-v", CUT_CONTEXT, &["apply", "-v", "--recount", "--verbose"]);
    same("added-recount", CUT_ADDED, &["apply", "--recount", "--stat"]);
}
