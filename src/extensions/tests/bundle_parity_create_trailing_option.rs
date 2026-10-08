//! `git bundle create <file> … --version=3`: an option after the `<file>` operand.
//!
//! `create`'s options are `PARSE_OPT_STOP_AT_NON_OPTION`, so everything after
//! `<file>` goes to `setup_revisions()`, and what it cannot place is
//! `ret = error(_("unrecognized argument: %s"), argv[1]); goto out;`
//! (bundle.c:513-516). Stock git 2.55.0 prints that line and then dies of
//! SIGABRT: `out:` runs `object_array_clear(&revs_copy.pending)` (:600) on a
//! `revs_copy` that is only initialised at :551. The port dies the same way:
//! the message, SIGABRT and the absence of any bundle are what this pins. The
//! abort was measured on git 2.54.0, 2.55.0 and 2.56.0.
#![cfg(unix)]

use std::path::PathBuf;
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-bundleopt-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        f.ok(&["init", "-q", "-b", "main"]);
        std::fs::write(f.root.join("a"), "a\n").unwrap();
        f.ok(&["add", "a"]);
        f.ok(&["commit", "-q", "-m", "a"]);
        f
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
            .env("LC_ALL", "C")
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) {
        assert!(self.run(args).status.success(), "git {args:?} failed");
    }
}

/// How `bundle create` ends after reporting an unclaimed word. Stock frees an
/// uninitialised `revs_copy` there (bundle.c:600): undefined behaviour, so macOS's
/// allocator aborts (SIGABRT) while glibc lets it pass and git exits 1 (measured on
/// the ubuntu CI runner with git 2.56.0).
fn assert_unclaimed_word_exit(out: &std::process::Output) {
    if cfg!(target_vendor = "apple") {
        assert_eq!(out.status.signal(), Some(6), "SIGABRT, as stock");
    } else {
        assert_eq!((out.status.code(), out.status.signal()), (Some(1), None), "exit 1, as stock");
    }
}

#[test]
fn a_version_after_the_file_is_an_unrecognized_revision_argument() {
    let f = Fixture::new("stdout");
    let out = f.run(&["bundle", "create", "-", "--all", "--version=3"]);
    assert_eq!(String::from_utf8_lossy(&out.stderr), "error: unrecognized argument: --version=3\n");
    assert!(out.stdout.is_empty(), "no bundle bytes on stdout");
    assert_unclaimed_word_exit(&out);
}

#[test]
fn no_bundle_file_is_written() {
    let f = Fixture::new("file");
    let out = f.run(&["bundle", "create", "b.bundle", "main", "-q"]);
    assert_eq!(String::from_utf8_lossy(&out.stderr), "error: unrecognized argument: -q\n");
    assert_unclaimed_word_exit(&out);
    assert!(!f.root.join("b.bundle").exists());
}
