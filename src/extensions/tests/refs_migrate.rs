//! `git refs migrate` — the checks `cmd_refs_migrate()` (builtin/refs.c) makes before
//! `repo_migrate_ref_storage_format()` is ever called.
//!
//! Two exit codes are in play and they are easy to confuse: `usage()` exits 129 and
//! prints only its message, while `error()` returns `-1` up through `cmd_refs()`, which
//! the process truncates to 255.
//!
//! Expectations measured against stock git 2.55.0.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

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
        let root = std::env::temp_dir().join(format!("zvcs-refsmigrate-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        f
    }

    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

const USAGE: &str = "\
usage: git refs migrate --ref-format=<format> [--no-reflog] [--dry-run]

    --ref-format <format> specify the reference format to convert to
    --[no-]dry-run        perform a non-destructive dry-run
    --no-reflog           drop reflogs entirely during the migration
    --reflog              opposite of --no-reflog

";

#[test]
fn missing_ref_format_is_a_usage_error() {
    let f = Fixture::new("missing");
    // `--dry-run` alone still leaves `format_str` NULL, so the same check fires.
    for args in [&["refs", "migrate"][..], &["refs", "migrate", "--dry-run"][..]] {
        let (code, out, err) = f.run(args);
        assert_eq!(code, 129, "{args:?}");
        assert_eq!(out, "");
        assert_eq!(err, "usage: missing --ref-format=<format>\n", "{args:?}");
    }
}

/// The positional check runs *before* the missing-format check, so a stray argument
/// wins even when `--ref-format` was given.
#[test]
fn leftover_positionals_are_a_usage_error() {
    let f = Fixture::new("extra");
    for args in [
        &["refs", "migrate", "extra"][..],
        &["refs", "migrate", "--ref-format=files", "extra"][..],
    ] {
        let (code, out, err) = f.run(args);
        assert_eq!(code, 129, "{args:?}");
        assert_eq!(out, "");
        assert_eq!(err, "usage: too many arguments\n", "{args:?}");
    }
}

/// `error()` returns `-1`, which the process reports as 255 — not 1.
#[test]
fn unknown_format_names_report_error_and_255() {
    let f = Fixture::new("unknown");
    let (code, out, err) = f.run(&["refs", "migrate", "--ref-format=bogus"]);
    assert_eq!(code, 255);
    assert_eq!(out, "");
    assert_eq!(err, "error: unknown ref storage format 'bogus'\n");

    // `ref_storage_format_by_name()` matches case-sensitively.
    let (code, _, err) = f.run(&["refs", "migrate", "--ref-format=FILES"]);
    assert_eq!(code, 255);
    assert_eq!(err, "error: unknown ref storage format 'FILES'\n");
}

/// A fresh repository is already in `files` format, so the migration is refused
/// before any backend work is attempted. The separate-argument spelling of the
/// option has to reach the same place.
#[test]
fn migrating_to_the_current_format_is_refused() {
    let f = Fixture::new("same");
    for args in [
        &["refs", "migrate", "--ref-format=files"][..],
        &["refs", "migrate", "--ref-format", "files"][..],
    ] {
        let (code, out, err) = f.run(args);
        assert_eq!(code, 255, "{args:?}");
        assert_eq!(out, "");
        assert_eq!(err, "error: repository already uses 'files' format\n", "{args:?}");
    }
}

#[test]
fn help_goes_to_stdout_and_exits_129() {
    let f = Fixture::new("help");
    let (code, out, err) = f.run(&["refs", "migrate", "-h"]);
    assert_eq!(code, 129);
    assert_eq!(out, USAGE);
    assert_eq!(err, "");
}

/// `--ref-format` carries `PARSE_OPT_NONEG`, so `--no-ref-format` is simply unknown;
/// and a trailing `--ref-format` reports a missing value without the usage block.
#[test]
fn option_scan_errors_match_parse_options() {
    let f = Fixture::new("optscan");

    let (code, out, err) = f.run(&["refs", "migrate", "--no-ref-format"]);
    assert_eq!(code, 129);
    assert_eq!(out, "");
    assert_eq!(err, format!("error: unknown option `no-ref-format'\n{USAGE}"));

    let (code, out, err) = f.run(&["refs", "migrate", "--ref-format"]);
    assert_eq!(code, 129);
    assert_eq!(out, "");
    assert_eq!(
        err, "error: option `ref-format' requires a value\n",
        "this one prints no usage block"
    );
}
