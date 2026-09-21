//! `git grep`'s binary-file notice is emitted by `grep_source_1()` (grep.c:1706)
//! *before* any of `show_line()`'s decoration is reached, and it returns without
//! touching `opt->last_shown`. Three consequences this file pins, each of which
//! zvcs got wrong:
//!
//!   * `--heading` prints no heading for the binary file;
//!   * `--break` owes no blank line either before the notice or before the file
//!     that follows it (the notice never set `last_shown`);
//!   * the context-bearing modes (`-A`/`-B`/`-C`, `-W`) still print the notice —
//!     they are downstream of the `binary_match_only` early return, not instead
//!     of it;
//!
//! plus the name inside the notice is painted with `color.grep.filename`
//! (`output_color(..., opt->colors[GREP_COLOR_FILENAME])` on the same line).
//!
//! Measured against git 2.55.0 with `--threads 1`; the threaded default drops
//! the very first output line (`skip_first_line`, builtin/grep.c:1350), which is
//! a thread-scheduling artifact and not what the single-threaded spec says.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new(BIN).args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

/// A repo holding one binary file that matches and one text file that matches,
/// named so the binary one sorts first — the ordering that exposes the leaked
/// `--break`/`--heading` state on the *next* file.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-grepbin-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@e.x"]);
    git(&repo, &["config", "user.name", "t"]);
    // A NUL in the first chunk is what `grep_source_is_binary()` keys on.
    std::fs::write(repo.join("a.bin"), b"\x00needle here\n").unwrap();
    std::fs::write(repo.join("b.txt"), "one\nneedle\nthree\n").unwrap();
    git(&repo, &["add", "a.bin", "b.txt"]);
    git(&repo, &["commit", "-q", "-m", "c0"]);
    (repo, home)
}

fn grep(repo: &Path, home: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["grep", "--threads", "1"];
    args.extend_from_slice(extra);
    Command::new(BIN)
        .args(&args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .output()
        .unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn heading_skips_the_binary_file_and_leaves_the_next_heading_first() {
    let (repo, home) = fixture("heading");
    let d = stdout(&grep(&repo, &home, &["--heading", "needle"]));
    assert_eq!(
        d, "Binary file a.bin matches\nb.txt\nneedle\n",
        "the notice precedes show_line_header(), so a.bin gets no heading line:\n{d}"
    );
}

#[test]
fn file_break_owes_no_blank_line_around_the_notice() {
    let (repo, home) = fixture("brk");
    let d = stdout(&grep(&repo, &home, &["--break", "needle"]));
    // a.bin never set last_shown, so b.txt is still the first file to show a
    // line and its hunk carries no leading blank.
    assert_eq!(
        d, "Binary file a.bin matches\nb.txt:needle\n",
        "--break must not separate on a file that only printed the notice:\n{d}"
    );
}

#[test]
fn after_context_still_prints_the_notice_without_a_hunk_mark() {
    let (repo, home) = fixture("ctx");
    let d = stdout(&grep(&repo, &home, &["-A1", "needle"]));
    assert_eq!(
        d, "Binary file a.bin matches\nb.txt:needle\nb.txt-three\n",
        "-A1 is downstream of the binary early return, and owes no `--`:\n{d}"
    );
}

#[test]
fn function_context_still_prints_the_notice() {
    let (repo, home) = fixture("funcctx");
    let d = stdout(&grep(&repo, &home, &["-W", "needle", "--", "a.bin"]));
    assert_eq!(
        d, "Binary file a.bin matches\n",
        "-W must not swallow the binary notice:\n{d}"
    );
}

#[test]
fn notice_paints_the_name_with_the_filename_color() {
    let (repo, home) = fixture("color");
    let d = stdout(&grep(&repo, &home, &["--color=always", "needle", "--", "a.bin"]));
    assert_eq!(
        d, "Binary file \u{1b}[35ma.bin\u{1b}[m matches\n",
        "grep.c:1708 wraps the name in color.grep.filename:\n{d:?}"
    );
}

#[test]
fn tree_mode_notice_carries_the_rev_prefix_and_no_heading() {
    let (repo, home) = fixture("tree");
    let d = stdout(&grep(&repo, &home, &["--heading", "needle", "HEAD"]));
    assert_eq!(
        d, "Binary file HEAD:a.bin matches\nHEAD:b.txt\nneedle\n",
        "the rev-prefixed name is still just the name gs->name carries:\n{d}"
    );
}
