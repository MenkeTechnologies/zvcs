//! `git mktree`'s `die()` text goes through `vfreportf()` (usage.c:12-38), which
//! turns every control byte but tab and newline into `?` and clips the line to
//! 4095 bytes:
//!
//! ```c
//! for (; p != pend - 1 && *p; p++) {
//!         if (iscntrl(*p) && *p != '\t' && *p != '\n')
//!                 *p = '?';
//! }
//! ```
//!
//! zvcs wrote the input line back verbatim, so an escape byte in a malformed
//! record reached the terminal and a 5000-byte line was echoed whole.
//!
//! `--end-of-options` ends the options as `--` does (parse-options.c:1119-1124),
//! where zvcs refused it as an unknown option at 129.
//!
//! Expectations come from stock git (`support/stock_git.rs`) in an identical
//! repository.
#![cfg(unix)]

use std::io::Write;
use std::process::{Command, Stdio};

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

/// `git <args>` in `side`'s repository with `input` on stdin.
fn run(side: &twin_repo::Side, args: &[&str], input: &[u8]) -> (i32, Vec<u8>, Vec<u8>) {
    let mut child = Command::new(&side.bin)
        .args(args)
        .current_dir(side.repo())
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &side.root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let out = child.wait_with_output().unwrap();
    (out.status.code().unwrap_or(-1), out.stdout, out.stderr)
}

#[test]
fn control_bytes_and_long_lines_are_reported_as_vfreportf_writes_them() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("mktree-report", stock);
    let blob = String::from_utf8(s.git(&["rev-parse", "HEAD:a"]).stdout.into_bytes()).unwrap();
    let blob = blob.trim();
    let long = vec![b'x'; 5000];
    let inputs: Vec<Vec<u8>> = vec![
        // A backspace where the mode starts: `input format error: ?0644 …`.
        format!("\x08100644 blob {blob}\tab\n").into_bytes(),
        // An escape and a DEL inside the type word: `invalid object type "bl?b?"`.
        format!("100644 bl\x1bb\x7f {blob}\tx\n").into_bytes(),
        // A path with an escape after a slash: `path x/?y contains slash`.
        format!("100644 blob {blob}\tx/\x1by\n").into_bytes(),
        long,
    ];
    for input in &inputs {
        assert_eq!(run(&z, &["mktree"], input), run(&s, &["mktree"], input), "{input:?}");
    }
}

#[test]
fn end_of_options_ends_option_parsing() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("mktree-eoo", stock);
    let listing = s.git(&["ls-tree", "HEAD"]).stdout.into_bytes();
    for args in [
        &["mktree", "--end-of-options"][..],
        &["mktree", "--end-of-options", "--bogus"],
        &["mktree", "--batch", "--end-of-options", "-x"],
    ] {
        assert_eq!(run(&z, args, &listing), run(&s, args, &listing), "{args:?}");
    }
}
