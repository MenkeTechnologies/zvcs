//! `--inaccurate-eof` (apply.c:3111-3117) takes the final newline off the end of both images of a
//! hunk whose images both end in one, and the hunk is then placed by `match_fragment()` over the
//! shortened buffer. A hunk with trailing context may sit anywhere, the shortened last line
//! matching the file's as a prefix; a hunk without any (`match_end`) must end where the file does
//! (`current + preimage->buf.len == img->buf.len`), and the shortened buffer reaches the end only
//! when the file's last line has no newline — which is the case the flag exists for. Against a
//! final line that still has its newline git refuses such a hunk.
//!
//! zvcs matched the shortened line as a prefix wherever it stood, and applied the end-anchored
//! hunk to a file git leaves alone.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn apply(bin: &str, label: &str, file: &[u8], patch: &[u8], flags: &[&str]) -> (i32, String, Vec<u8>) {
    let root = std::env::temp_dir().join(format!("zvcs-apply-eof-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let dir: PathBuf = std::fs::canonicalize(&root).unwrap();
    std::fs::write(dir.join("f.txt"), file).unwrap();
    let mut child = Command::new(bin)
        .arg("apply")
        .args(flags)
        .current_dir(&dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(patch).unwrap();
    let out = child.wait_with_output().unwrap();
    let after = std::fs::read(Path::new(&dir).join("f.txt")).unwrap();
    let _ = std::fs::remove_dir_all(&root);
    (
        out.status.code().expect("no signal"),
        String::from_utf8_lossy(&out.stderr).replace(dir.to_str().unwrap(), "<dir>"),
        after,
    )
}

fn same(label: &str, file: &[u8], patch: &[u8], flags: &[&str]) {
    let Some(stock) = stock_git() else { return };
    let want = apply(stock, &format!("{label}-stock"), file, patch, flags);
    let got = apply(ZVCS, &format!("{label}-zvcs"), file, patch, flags);
    assert_eq!(got, want, "{label} {flags:?}: left is zvcs, right is stock");
}

fn header(hunk: &str) -> Vec<u8> {
    format!("diff --git a/f.txt b/f.txt\n--- a/f.txt\n+++ b/f.txt\n{hunk}").into_bytes()
}

const WITH_NL: &[u8] = b"a\nb\nc\nd\n";
const WITHOUT_NL: &[u8] = b"a\nb\nc\nd";

#[test]
fn an_end_anchored_hunk_needs_a_final_line_without_a_newline() {
    // Pre-image `d\n`, post-image `d\ne\n`; no trailing context, so it must match the end.
    let patch = header("@@ -4 +4,2 @@\n d\n+e\n");
    for flags in [&["--inaccurate-eof"][..], &[], &["--check", "--inaccurate-eof"]] {
        same("end-ctx-nl", WITH_NL, &patch, flags);
        same("end-ctx-nonl", WITHOUT_NL, &patch, flags);
    }
}

#[test]
fn a_hunk_with_trailing_context_may_sit_anywhere() {
    // `-b +B` with `c`, `d` after: the shortened last line is `d`, and match_end is off.
    let patch = header("@@ -2,3 +2,3 @@\n-b\n+B\n c\n d\n");
    for flags in [&["--inaccurate-eof"][..], &[]] {
        same("trailing-nl", WITH_NL, &patch, flags);
        same("trailing-nonl", WITHOUT_NL, &patch, flags);
    }
}

#[test]
fn the_shortened_line_in_the_middle_of_a_file_follows_the_same_rules() {
    let patch = header("@@ -1,2 +1,3 @@\n a\n b\n+x\n");
    same("mid-nl", WITH_NL, &patch, &["--inaccurate-eof"]);
    same("mid-nonl", WITHOUT_NL, &patch, &["--inaccurate-eof"]);
    let patch = header("@@ -3,2 +3,2 @@\n-c\n+C\n d\n");
    same("last-nl", WITH_NL, &patch, &["--inaccurate-eof"]);
    same("last-nonl", WITHOUT_NL, &patch, &["--inaccurate-eof"]);
}

#[test]
fn an_accurate_marker_is_not_touched() {
    let patch = header("@@ -4 +4 @@\n-d\n\\ No newline at end of file\n+D\n\\ No newline at end of file\n");
    same("marker-nonl", WITHOUT_NL, &patch, &["--inaccurate-eof"]);
    same("marker-nl", WITH_NL, &patch, &["--inaccurate-eof"]);
}

#[test]
fn a_hunk_that_changes_the_last_line_with_no_marker() {
    let patch = header("@@ -4 +4 @@\n-d\n+D\n");
    same("change-nl", WITH_NL, &patch, &["--inaccurate-eof"]);
    same("change-nonl", WITHOUT_NL, &patch, &["--inaccurate-eof"]);
}
