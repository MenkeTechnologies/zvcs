//! Three things bare operands mean to `git rev-parse`.
//!
//! * With no arguments at all it is a repository test:
//!   `if (argc == 1) { setup_git_directory(the_repository); … return 0; }`
//!   (builtin/rev-parse.c:740-745, v2.55.0). The status is the answer, and the
//!   setup it runs is the one that reports a `.git` file it cannot follow
//!   (setup.c:1600-1634, with `die_on_error = 1` from :1951). The port answered
//!   0 either way, so the form was useless as the test it exists to be.
//! * `..` is not a range: `try_difference()` returns 0 when both endpoints
//!   defaulted and the separator was not symmetric, because "Just `..`? That is
//!   not a range but the pathspec for the parent directory"
//!   (builtin/rev-parse.c:292-300). The port answered `HEAD..HEAD`.
//! * A pathspec that cannot be in the file system is accepted as one:
//!   `verify_filename()` is
//!   `if (looks_like_pathspec(arg) || check_filename(prefix, arg)) return;`
//!   (setup.c:289-290). Only the second half was consulted, so
//!   `git rev-parse '*.c'` echoed the pathspec and then died about it.
//!
//! Every expectation was measured from stock git 2.55.0 in an identical
//! throwaway repository, stdout, stderr and exit status compared separately.
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
    /// One commit on `main`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-rp-operands-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&f.work, &["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file.txt"), "a\n").unwrap();
        f.git(&f.work, &["add", "file.txt"]);
        f.git(&f.work, &["commit", "-q", "-m", "subject"]);
        f
    }

    fn cmd(&self, dir: &PathBuf, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        c
    }

    fn git(&self, dir: &PathBuf, args: &[&str]) {
        let out = self.cmd(dir, args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn run(&self, dir: &PathBuf, args: &[&str]) -> (String, String, i32) {
        let out = self.cmd(dir, args).output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn oid(&self, spec: &str) -> String {
        let work = self.work.clone();
        let (out, err, code) = self.run(&work, &["rev-parse", spec]);
        assert_eq!((err.as_str(), code), ("", 0), "{spec}");
        out.trim().to_string()
    }
}

/// Inside a repository the no-argument form is silent and succeeds; outside one
/// it is whatever setup dies with.
#[test]
fn the_no_argument_form_is_the_repository_test() {
    let f = Fixture::new("plain");
    let work = f.work.clone();
    assert_eq!(f.run(&work, &["rev-parse"]), (String::new(), String::new(), 0));

    let outside = f.root.clone();
    let (out, err, code) = f.run(&outside, &["rev-parse"]);
    assert_eq!((out.as_str(), code), ("", 128));
    assert!(err.contains("not a git repository"), "{err:?}");
}

/// A `.git` file with no `gitdir:` line, and one naming a directory that is not
/// a repository: `read_gitfile_error_die()`'s two spellings, from the walk.
#[test]
fn a_git_file_that_cannot_be_followed_ends_the_no_argument_form() {
    let f = Fixture::new("gitfile");
    let real = f.work.join(".git");
    let outer = f.root.join("outer");
    std::fs::create_dir_all(&outer).unwrap();

    std::fs::write(outer.join(".git"), format!("gitdir {}\n", real.display())).unwrap();
    let (out, err, code) = f.run(&outer, &["rev-parse"]);
    assert_eq!((out.as_str(), code), ("", 128));
    assert!(err.starts_with("fatal: invalid gitfile format: "), "{err:?}");

    std::fs::write(outer.join(".git"), format!("gitdir: {}.nope\n", real.display())).unwrap();
    let (out, err, code) = f.run(&outer, &["rev-parse"]);
    assert_eq!((out.as_str(), code), ("", 128));
    assert!(err.starts_with("fatal: not a git repository: "), "{err:?}");

    // And a gitfile that *can* be followed is the ordinary success.
    std::fs::write(outer.join(".git"), format!("gitdir: {}\n", real.display())).unwrap();
    assert_eq!(f.run(&outer, &["rev-parse"]), (String::new(), String::new(), 0));
}

/// `..` is the parent directory, while every other spelling around it stays a
/// range.
#[test]
fn a_bare_dotdot_is_a_pathspec_and_not_an_empty_range() {
    let f = Fixture::new("dotdot");
    let work = f.work.clone();
    let head = f.oid("HEAD");

    let (out, err, code) = f.run(&work, &["rev-parse", ".."]);
    assert_eq!((out.as_str(), err.as_str(), code), ("..\n", "", 0));

    let range = format!("{head}\n^{head}\n");
    assert_eq!(f.run(&work, &["rev-parse", "HEAD.."]).0, range);
    assert_eq!(f.run(&work, &["rev-parse", "..HEAD"]).0, range);
    // `...` is symmetric, so the guard does not fire: both endpoints and the
    // merge base of HEAD with itself.
    assert_eq!(
        f.run(&work, &["rev-parse", "..."]).0,
        format!("{head}\n{head}\n^{head}\n")
    );
}

/// A wildcard operand is a pathspec that need not exist; one without a wildcard
/// that does not exist is still the ambiguous-argument failure.
#[test]
fn a_wildcard_operand_is_accepted_as_a_pathspec() {
    let f = Fixture::new("wildcard");
    let work = f.work.clone();
    assert_eq!(
        f.run(&work, &["rev-parse", "*.c"]),
        ("*.c\n".to_string(), String::new(), 0)
    );
    assert_eq!(
        f.run(&work, &["rev-parse", ":(glob)nope/**"]),
        (":(glob)nope/**\n".to_string(), String::new(), 0)
    );

    let (out, err, code) = f.run(&work, &["rev-parse", "nope.c"]);
    assert_eq!((out.as_str(), code), ("nope.c\n", 128));
    assert!(err.contains("ambiguous argument 'nope.c'"), "{err:?}");
}
