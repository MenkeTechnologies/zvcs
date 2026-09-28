//! `format-patch -o` makes its directory up front and reports failures as git.
//!
//! `cmd_format_patch()` runs `safe_create_leading_directories_const()` and
//! `mkdir()` on the output directory before the walk (builtin/log.c:2272-2284),
//! so an empty series still leaves the directory behind, and a failed `mkdir()`
//! is `die_errno("could not create directory '%s'")` naming the directory as
//! `set_outdir()` built it from the top of the worktree. `open_next_file()`
//! (builtin/log.c:1140-1168) prints each name and only then opens the file; a
//! failed `fopen()` is `error_errno("cannot open patch file %s")` and the caller
//! dies with `failed to create output files`, or `failed to create cover-letter
//! file` for the cover letter. `strbuf_complete()` joins the directory with one
//! slash. zvcs created the directory lazily and surfaced the raw I/O error.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::{Path, PathBuf};
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
    /// Three commits to the file `a`, whose last is `three`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-format-patch-outdir-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(work.join("s")).unwrap();
        let f = Fixture { root, work };
        f.run(&f.work, &["init", "-q", "-b", "main", "."]);
        for (n, msg) in [("1", "one"), ("2", "two"), ("3", "three")] {
            std::fs::write(f.work.join("a"), format!("{n}\n")).unwrap();
            f.run(&f.work, &["add", "a"]);
            f.run(&f.work, &["commit", "-q", "-m", msg]);
        }
        f
    }

    fn run(&self, dir: &Path, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "a@e.x")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "c@e.x")
            .env("GIT_AUTHOR_DATE", "1112911993 -0700")
            .env("GIT_COMMITTER_DATE", "1112911993 -0700")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

#[test]
fn the_directory_is_made_before_the_walk() {
    let f = Fixture::new("empty");
    assert_eq!(f.run(&f.work, &["format-patch", "-o", "nd", "HEAD..HEAD"]), (String::new(), String::new(), 0));
    assert!(f.work.join("nd").is_dir());
    // One slash between a directory that already ends in one and the name.
    std::fs::create_dir(f.work.join("d2")).unwrap();
    assert_eq!(f.run(&f.work, &["format-patch", "-o", "d2/", "-1"]).0, "d2/0001-three.patch\n");
}

#[test]
fn a_directory_under_a_file_is_refused_by_name() {
    let f = Fixture::new("mkdir");
    assert_eq!(
        f.run(&f.work, &["format-patch", "-o", "a/b", "-1"]),
        (String::new(), "fatal: could not create directory 'a/b': Not a directory\n".to_owned(), 128)
    );
    // From a subdirectory the name is the one `set_outdir()` built from the top.
    assert_eq!(
        f.run(&f.work.join("s"), &["format-patch", "-o", "../a/x", "-1"]),
        (
            String::new(),
            "fatal: could not create directory 's/../a/x': Not a directory\n".to_owned(),
            128
        )
    );
}

#[test]
fn a_patch_file_that_cannot_be_opened_stops_the_run() {
    let f = Fixture::new("open");
    assert_eq!(
        f.run(&f.work, &["format-patch", "-o", "a", "-1"]),
        (
            "a/0001-three.patch\n".to_owned(),
            "error: cannot open patch file a/0001-three.patch: Not a directory\n\
             fatal: failed to create output files\n"
                .to_owned(),
            128
        )
    );
    assert_eq!(
        f.run(&f.work, &["format-patch", "-o", "a", "--cover-letter", "-1"]),
        (
            "a/0000-cover-letter.patch\n".to_owned(),
            "error: cannot open patch file a/0000-cover-letter.patch: Not a directory\n\
             fatal: failed to create cover-letter file\n"
                .to_owned(),
            128
        )
    );
}
