//! `--index-version` / `--show-index-version`: the version an index that was
//! never read reports, and the line `--verbose` prints when the version is set.
//!
//! ```c
//! if (preferred_index_format) {
//!         if (preferred_index_format < 0) {
//!                 printf(_("%d\n"), the_repository->index->version);
//!         } else if (preferred_index_format < INDEX_FORMAT_LB ||
//!                    INDEX_FORMAT_UB < preferred_index_format) {
//!                 die("index-version %d not in range: %d..%d", …);
//!         } else {
//!                 if (the_repository->index->version != preferred_index_format)
//!                         the_repository->index->cache_changed |= SOMETHING_CHANGED;
//!                 report(_("index-version: was %d, set to %d"),
//!                        the_repository->index->version, preferred_index_format);
//!                 the_repository->index->version = preferred_index_format;
//!         }
//! }
//! ```
//!
//! (builtin/update-index.c:1182-1196.) Two things follow that are easy to get
//! wrong. `report()` is the `--verbose` printer, so the `was N, set to M` line is
//! part of the command's output and not a debugging aside. And the `N` it names
//! is `istate->version`, which `do_read_index()` only ever assigns from an
//! on-disk header (read-cache.c:2245) — a repository whose `$GIT_DIR/index` does
//! not exist yet still holds the zero it was allocated with, so git prints `0`
//! and not the version it is about to write.
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
    /// An initialised repository with *no* index file yet.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-uiver-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.ok(&["init", "-q", "-b", "main", "."]);
        assert!(!f.work.join(".git/index").exists(), "fixture must start index-less");
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        c
    }

    fn ok(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    /// `(stdout, stderr)` of a command that must succeed.
    fn run(&self, args: &[&str]) -> (String, String) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn add_a_file(&self) {
        std::fs::write(self.work.join("a"), b"a\n").unwrap();
        self.ok(&["update-index", "--add", "a"]);
    }
}

#[test]
fn unborn_index_reports_version_zero() {
    let f = Fixture::new("show0");
    assert_eq!(f.run(&["update-index", "--show-index-version"]).0, "0\n");
}

#[test]
fn an_index_that_was_read_reports_its_header_version() {
    let f = Fixture::new("show2");
    f.add_a_file();
    assert_eq!(f.run(&["update-index", "--show-index-version"]).0, "2\n");

    f.ok(&["update-index", "--index-version", "4"]);
    assert_eq!(f.run(&["update-index", "--show-index-version"]).0, "4\n");
}

#[test]
fn verbose_reports_the_version_transition() {
    let f = Fixture::new("report");
    f.add_a_file();

    let (out, _) = f.run(&["update-index", "--verbose", "--index-version", "4"]);
    assert_eq!(out, "index-version: was 2, set to 4\n");

    // Re-stating the version the index already has still reports; only the
    // `SOMETHING_CHANGED` guard cares about the difference.
    let (out, _) = f.run(&["update-index", "--verbose", "--index-version", "4"]);
    assert_eq!(out, "index-version: was 4, set to 4\n");

    let (out, _) = f.run(&["update-index", "--verbose", "--index-version", "2"]);
    assert_eq!(out, "index-version: was 4, set to 2\n");
}

/// The zero of an unborn index reaches the report too, where it is at its most
/// visible: git says `was 0` even though the index it writes is a version 2.
#[test]
fn verbose_reports_zero_for_an_unborn_index() {
    let f = Fixture::new("report0");
    let (out, _) = f.run(&["update-index", "--verbose", "--index-version", "2"]);
    assert_eq!(out, "index-version: was 0, set to 2\n");
    assert_eq!(f.run(&["update-index", "--show-index-version"]).0, "2\n");
}

/// Without `--verbose` the transition is silent — `report()` is the verbose
/// printer, so restoring the line must not have made it unconditional.
#[test]
fn the_transition_is_silent_without_verbose() {
    let f = Fixture::new("quiet");
    f.add_a_file();
    assert_eq!(f.run(&["update-index", "--index-version", "4"]), (String::new(), String::new()));
}
