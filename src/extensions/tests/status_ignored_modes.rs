//! At which granularity `git status` lists ignored paths.
//!
//! `wt_status_collect_untracked()` sets `DIR_SHOW_OTHER_DIRECTORIES` only when the
//! untracked mode is *not* `all`, and adds `DIR_SHOW_IGNORED_TOO_MODE_MATCHING` only
//! for `--ignored=matching`. That gives three shapes:
//!
//! * `--ignored` with the default `-unormal`: an ignored directory is one entry.
//! * `--ignored` with `-uall`: the walk descends into it and names every file.
//! * `--ignored=matching`: whatever the pattern matched is what is reported — the
//!   directory when a directory pattern matched it, the file when a file pattern did.
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
    /// `build/` is ignored as a directory, `*.log` as a file pattern, and `plain/` is
    /// untracked but not ignored at all.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-ignmodes-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f.write(".gitignore", b"build/\n*.log\n");
        f.write("tracked.txt", b"r\n");
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "init"]);
        f.write("build/output.o", b"o\n");
        f.write("logs/debug.log", b"d\n");
        f.write("plain/p.txt", b"p\n");
        f.write("untracked.txt", b"u\n");
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

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    fn write(&self, path: &str, body: &[u8]) {
        if let Some(parent) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(self.work.join(parent)).unwrap();
        }
        std::fs::write(self.work.join(path), body).unwrap();
    }

    /// Status lines, sorted so the walk order cannot make the test flaky.
    fn status(&self, args: &[&str]) -> Vec<String> {
        let mut argv = vec!["status"];
        argv.extend_from_slice(args);
        let out = self.cmd(&argv).output().unwrap();
        assert!(out.status.success(), "`git {argv:?}` failed: {out:?}");
        let mut lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(ToOwned::to_owned)
            .collect();
        lines.sort();
        lines
    }
}

#[test]
fn the_default_granularity_collapses_ignored_directories() {
    let f = Fixture::new("normal");
    assert_eq!(
        f.status(&["--porcelain", "--ignored"]),
        ["!! build/", "!! logs/", "?? plain/", "?? untracked.txt"]
    );
}

#[test]
fn untracked_all_names_every_ignored_file() {
    let f = Fixture::new("all");
    assert_eq!(
        f.status(&["--porcelain", "-uall", "--ignored"]),
        [
            "!! build/output.o",
            "!! logs/debug.log",
            "?? plain/p.txt",
            "?? untracked.txt"
        ]
    );
    // The v2 format is fed by the same walk.
    assert!(
        f.status(&["--porcelain=v2", "-uall", "--ignored"])
            .contains(&"! build/output.o".to_string())
    );
}

#[test]
fn matching_reports_what_the_pattern_matched() {
    let f = Fixture::new("matching");
    // `build/` matched a directory pattern; `debug.log` matched a file pattern.
    assert_eq!(
        f.status(&["--porcelain", "--ignored=matching"]),
        ["!! build/", "!! logs/debug.log", "?? plain/", "?? untracked.txt"]
    );
    assert_eq!(
        f.status(&["--porcelain", "--ignored=matching", "-uall"]),
        [
            "!! build/",
            "!! logs/debug.log",
            "?? plain/p.txt",
            "?? untracked.txt"
        ]
    );
}

/// Without `--ignored` nothing about ignored paths is printed, whatever `-u` says.
#[test]
fn ignored_paths_stay_hidden_by_default() {
    let f = Fixture::new("hidden");
    assert_eq!(
        f.status(&["--porcelain", "-uall"]),
        ["?? plain/p.txt", "?? untracked.txt"]
    );
}
