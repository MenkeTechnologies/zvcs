//! `git get-tar-commit-id`'s short-read failure is a `die_errno`, not a `die`.
//!
//! ```c
//! n = read_in_full(0, buffer, HEADERSIZE);
//! if (n < 0)
//!         die_errno("git get-tar-commit-id: read error");
//! if (n != HEADERSIZE)
//!         die_errno("git get-tar-commit-id: EOF before reading tar header");
//! ```
//! (`builtin/get-tar-commit-id.c:35-39`)
//!
//! `die_errno` always appends the live `errno`:
//!
//! ```c
//! err = strerror(errno);
//! …
//! snprintf(buf, n, "%s: %s", fmt, str_error);
//! ```
//! (`usage.c:220,235`)
//!
//! Nothing on this path *sets* `errno`. A clean EOF makes `read_in_full()` return
//! a short count from a `read(2)` that succeeded, so what git reports is whatever
//! its own startup left behind. Observed from stock git 2.55.0 on the same
//! machine, same input, differing only in the working directory:
//!
//! ```text
//! $ cd /tmp && git get-tar-commit-id </dev/null
//! fatal: git get-tar-commit-id: EOF before reading tar header: No such file or directory
//! $ cd <a long path> && git get-tar-commit-id </dev/null
//! fatal: git get-tar-commit-id: EOF before reading tar header: Result too large
//! ```
//!
//! (`ERANGE` there is `strbuf_getcwd()`'s first `getcwd(3)` having had to grow its
//! buffer; `ENOENT` is a config or discovery `open()` that missed.) The tail is a
//! property of the process that printed it, not of the input — so this file
//! asserts its *shape*: present, separated by exactly `": "`, rendered as
//! `strerror` renders it, and free of the ` (os error N)` suffix Rust's own
//! `io::Error` `Display` would have added. Hard-coding the word would pin one
//! libc's spelling of one accident and fail on the next platform.
#![cfg(unix)]

use std::io::Write as _;
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

const EOF_PREFIX: &str = "fatal: git get-tar-commit-id: EOF before reading tar header";

/// Run the binary under test with `input` on stdin, outside any repository.
fn run(input: &[u8]) -> Output {
    let dir = std::env::temp_dir();
    let mut child = Command::new(BIN)
        .arg("get-tar-commit-id")
        .current_dir(&dir)
        .env("HOME", &dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

/// The errno tail of an EOF fatal: everything after `": "`, with the newline off.
///
/// Asserts the fixed part of the line as it goes, so a caller only has to reason
/// about the part that legitimately varies.
fn errno_tail(out: &Output) -> String {
    assert_eq!(out.status.code(), Some(128), "`die_errno` exits 128");
    let text = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(text.lines().count(), 1, "one fatal line, got: {text:?}");
    assert!(text.ends_with('\n'), "the fatal line must be terminated: {text:?}");
    let line = text.trim_end_matches('\n');
    let tail = line
        .strip_prefix(EOF_PREFIX)
        .unwrap_or_else(|| panic!("wrong fatal text: {line:?}"))
        .strip_prefix(": ")
        .unwrap_or_else(|| panic!("`die_errno` must append `: <strerror>`, got: {line:?}"));
    assert!(
        !tail.is_empty(),
        "`strerror` never returns an empty string, not even for errno 0: {line:?}"
    );
    // Rust renders the same `io::Error` as `No such file or directory (os error 2)`.
    // Those eleven characters are Rust's, not git's, and their presence is the
    // exact regression this file exists to catch.
    assert!(!tail.contains("(os error"), "Rust's `io::Error` suffix leaked: {line:?}");
    // `strerror` text is a bare sentence — no trailing punctuation, no wrapping.
    assert!(!tail.starts_with(' ') && !tail.ends_with(' '), "stray padding: {line:?}");
    tail.to_string()
}

#[test]
fn empty_stdin_reports_an_errno_after_the_eof_message() {
    // The plain case: stdin is at EOF before a single byte arrives.
    let out = run(b"");
    let tail = errno_tail(&out);
    assert!(out.stdout.is_empty(), "the fatal path writes nothing to stdout");
    eprintln!("errno tail observed: {tail}");
}

#[test]
fn a_partial_header_reports_the_same_way_a_missing_one_does() {
    // `read_in_full` returning *anything* short of 1024 takes the same branch, so
    // one byte and 1023 bytes must both produce the errno-tailed fatal.
    for len in [1usize, 511, 512, 1023] {
        let out = run(&vec![b'x'; len]);
        errno_tail(&out);
        assert!(out.stdout.is_empty(), "{len}: the fatal path writes nothing to stdout");
    }
}

#[test]
fn a_full_header_never_reaches_the_errno_path() {
    // The guard against over-eager erroring: exactly 1024 bytes is a complete
    // read, so a non-`g` typeflag is the silent `return 1` and not a fatal
    // (`builtin/get-tar-commit-id.c:40-41`).
    let out = run(&vec![0u8; 1024]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert!(out.stderr.is_empty(), "a short-header exit must be silent: {out:?}");
}

#[test]
fn a_pax_global_header_still_round_trips_a_commit_id() {
    // The success path, kept beside the failure path so a change to the read loop
    // cannot quietly break extraction while the error text keeps passing.
    let id = "0123456789abcdef0123456789abcdef01234567";
    let mut buf = vec![0u8; 1024];
    // `typeflag` sits at offset 156 of the ustar header; `g` is
    // `TYPEFLAG_GLOBAL_HEADER`.
    buf[156] = b'g';
    // 52 = the length field itself and `" comment="` (11 bytes) + 40 hex + LF.
    let record = format!("52 comment={id}\n");
    buf[512..512 + record.len()].copy_from_slice(record.as_bytes());

    let out = run(&buf);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), format!("{id}\n"));
    assert!(out.stderr.is_empty());
}
