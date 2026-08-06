//! `-e <pattern>` protects files from `git clean`, `-x` included.
//!
//! `cmd_clean()` adds every `-e` pattern to the `EXC_CMDL` group whatever else was
//! asked for, and only `-x` makes it skip `setup_standard_excludes()`. So under `-x`
//! the command-line patterns are the *only* thing left that can hold a file back —
//! and a directory holding one of them is reported file-by-file, because removing the
//! directory would take the excluded file with it.
//!
//! Losing that is data loss, not a formatting difference: `git clean -fdx -e '*.tmp'`
//! must leave every `*.tmp` on disk.
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
    /// A repository with `*.log` ignored, one tracked file, and a mix of untracked,
    /// ignored and to-be-excluded files — `logs/` deliberately holds one of each.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-cleanexclude-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(work.join("logs")).unwrap();
        std::fs::create_dir_all(work.join("pure")).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f.write(".gitignore", b"*.log\n");
        f.write("tracked.txt", b"r\n");
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "init"]);
        f.write("logs/debug.log", b"a\n"); // ignored by `.gitignore`
        f.write("logs/keep.tmp", b"b\n"); // excluded by `-e` below
        f.write("pure/keep.tmp", b"c\n"); // ditto, and the only thing in `pure/`
        f.write("top.tmp", b"d\n"); // ditto, at the top level
        f.write("plain.txt", b"e\n"); // plainly untracked
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
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn write(&self, path: &str, body: &[u8]) {
        std::fs::write(self.work.join(path), body).unwrap();
    }

    /// `clean` output as a sorted list, so walk order cannot make the test flaky.
    fn clean(&self, args: &[&str]) -> Vec<String> {
        let mut argv = vec!["clean"];
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

    fn exists(&self, path: &str) -> bool {
        self.work.join(path).exists()
    }
}

/// The excluded files are neither listed nor removed, and the directory that holds
/// one of them is reported by its removable contents instead of as a whole.
#[test]
fn exclude_survives_the_ignored_sweep() {
    let f = Fixture::new("dryrun");
    assert_eq!(
        f.clean(&["-nd", "-x", "-e", "*.tmp"]),
        ["Would remove logs/debug.log", "Would remove plain.txt"]
    );
}

/// The real thing: nothing matching `-e` may be gone afterwards.
#[test]
fn a_forced_sweep_leaves_the_excluded_files_on_disk() {
    let f = Fixture::new("force");
    f.git(&["clean", "-fdx", "-e", "*.tmp"]);

    assert!(f.exists("logs/keep.tmp"), "excluded file inside a directory");
    assert!(f.exists("pure/keep.tmp"), "excluded file in a directory of its own");
    assert!(f.exists("top.tmp"), "excluded file at the top level");
    assert!(f.exists("tracked.txt"), "tracked files are never touched");
    assert!(!f.exists("plain.txt"), "an untracked file still goes");
    assert!(!f.exists("logs/debug.log"), "`-x` still removes ignored files");
}

/// A later negation gives the protection back up, so the directory collapses again.
#[test]
fn a_negated_pattern_undoes_the_exclusion() {
    let f = Fixture::new("negated");
    assert_eq!(
        f.clean(&["-nd", "-x", "-e", "*.tmp", "-e", "!logs/keep.tmp"]),
        ["Would remove logs/", "Would remove plain.txt"]
    );
}

/// `-X` deletes exactly what the ignore rules match, and a `-e` pattern only adds to
/// them — it protects nothing there.
#[test]
fn exclude_does_not_protect_under_ignored_only() {
    let f = Fixture::new("ignored-only");
    assert_eq!(
        f.clean(&["-nd", "-X", "-e", "*.tmp"]),
        ["Would remove logs/", "Would remove pure/", "Would remove top.tmp"]
    );
}

/// Without `-x` the repository's own ignore rules still hide `logs/debug.log`, every
/// other untracked path is a target, and `-e` keeps subtracting from that set.
#[test]
fn the_plain_sweep_is_unchanged() {
    let f = Fixture::new("plain");
    assert_eq!(
        f.clean(&["-nd"]),
        [
            "Would remove logs/keep.tmp",
            "Would remove plain.txt",
            "Would remove pure/",
            "Would remove top.tmp"
        ]
    );
    assert_eq!(
        f.clean(&["-nd", "-e", "plain.txt"]),
        ["Would remove logs/keep.tmp", "Would remove pure/", "Would remove top.tmp"]
    );
}
