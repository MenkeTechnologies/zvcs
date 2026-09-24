//! How `git mv` spells each destination it records.
//!
//! `cmd_mv()` builds destinations with `internal_prefix_pathspec()` and
//! `add_slash()` (builtin/mv.c:54-93, :254-281): sources lose their trailing
//! slashes, the destination keeps its own unless the move is `dir no-such-dir/`,
//! an existing directory (by `lstat()`) gets each source's basename under exactly
//! one `/`, and `.` (normalised to the empty string) puts the basename at the
//! top. Joining with `format!("{dest}/{base}")` instead recorded `dir//a` in the
//! index for `git mv a dir/` and left the real `dir/a` untracked.
//!
//! Expectations measured against stock git 2.55.0.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output};

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
    /// `README.md`, `src/x`, `nested/deep/y`, `sub/f`, `sub/inner/z` tracked,
    /// plus `lnk`, a tracked symlink to `src`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-mvdest-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        for d in ["src", "nested/deep", "sub/inner"] {
            std::fs::create_dir_all(work.join(d)).unwrap();
        }
        let f = Fixture { root, work };
        for (p, body) in [
            ("README.md", "a\n"),
            ("src/x", "b\n"),
            ("nested/deep/y", "c\n"),
            ("sub/f", "d\n"),
            ("sub/inner/z", "e\n"),
        ] {
            std::fs::write(f.work.join(p), body).unwrap();
        }
        std::os::unix::fs::symlink("src", f.work.join("lnk")).unwrap();
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["add", "-A"]);
        f.git(&["-c", "user.name=t", "-c", "user.email=t@e.co", "commit", "-q", "-m", "init"]);
        f
    }

    fn cmd(&self, dir: &str, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(self.work.join(dir))
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(".", args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn run_in(&self, dir: &str, args: &[&str]) -> Output {
        self.cmd(dir, args).output().unwrap()
    }

    fn tracked(&self) -> Vec<String> {
        let out = self.run_in(".", &["ls-files"]);
        String::from_utf8_lossy(&out.stdout).lines().map(ToOwned::to_owned).collect()
    }

    fn untracked(&self) -> String {
        let out = self.run_in(".", &["ls-files", "--others", "--exclude-standard"]);
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// `(dir, args, index after the move)` for every destination spelling that
/// lands; each one leaves nothing untracked behind.
#[test]
fn destinations_are_recorded_with_one_separator() {
    let cases: &[(&str, &[&str], &[&str])] = &[
        (".", &["mv", "README.md", "src/"], &["lnk", "nested/deep/y", "src/README.md", "src/x", "sub/f", "sub/inner/z"]),
        (".", &["mv", "README.md", "nested/deep/"], &["lnk", "nested/deep/README.md", "nested/deep/y", "src/x", "sub/f", "sub/inner/z"]),
        (".", &["mv", "src/x", "."], &["README.md", "lnk", "nested/deep/y", "sub/f", "sub/inner/z", "x"]),
        (".", &["mv", "src/x", "./"], &["README.md", "lnk", "nested/deep/y", "sub/f", "sub/inner/z", "x"]),
        // A source's trailing slash goes; the directory lands under its basename.
        (".", &["mv", "src/", "nested/deep/"], &["README.md", "lnk", "nested/deep/src/x", "nested/deep/y", "sub/f", "sub/inner/z"]),
        // `git mv dir no-such-dir/` is a rename, not a refusal.
        (".", &["mv", "src", "nosuch/"], &["README.md", "lnk", "nested/deep/y", "nosuch/x", "sub/f", "sub/inner/z"]),
        // From a subdirectory, `.` is the prefix itself.
        ("sub/inner", &["mv", "../f", "."], &["README.md", "lnk", "nested/deep/y", "src/x", "sub/inner/f", "sub/inner/z"]),
    ];
    for (dir, args, want) in cases {
        let f = Fixture::new("ok");
        let out = f.run_in(dir, args);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {out:?}");
        assert_eq!(f.tracked(), *want, "{args:?}");
        assert_eq!(f.untracked(), "", "{args:?}");
    }
}

/// A file into a directory that does not exist is a per-source `bad`
/// (builtin/mv.c:442): fatal without `-k`, skipped with it.
#[test]
fn missing_directory_destination_is_a_skippable_bad() {
    let f = Fixture::new("missing");
    let out = f.run_in(".", &["mv", "README.md", "nosuch/"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        stderr(&out),
        "fatal: destination directory does not exist, source=README.md, destination=nosuch/\n"
    );

    let out = f.run_in(".", &["mv", "-k", "README.md", "nosuch/"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(f.tracked(), ["README.md", "lnk", "nested/deep/y", "src/x", "sub/f", "sub/inner/z"]);
}

/// The destination test is `lstat()`: a symlink to a directory is an existing
/// file, not a directory to move into. `..` from `sub` folds to the top, where
/// the basename is the source itself.
#[test]
fn symlink_and_dotdot_destinations() {
    let f = Fixture::new("edge");
    let out = f.run_in(".", &["mv", "README.md", "lnk"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(stderr(&out), "fatal: destination exists, source=README.md, destination=lnk\n");

    let out = f.run_in("sub", &["mv", "../README.md", ".."]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        stderr(&out),
        "fatal: can not move directory into itself, source=README.md, destination=README.md\n"
    );
}

/// The dry run announces the normalised destination too.
#[test]
fn dry_run_names_the_joined_destination() {
    let f = Fixture::new("dry");
    let out = f.run_in(".", &["mv", "-n", "README.md", "src//"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "Checking rename of 'README.md' to 'src/README.md'\nRenaming README.md to src/README.md\n"
    );
}
