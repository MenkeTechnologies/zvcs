//! `git rev-parse --end-of-options`.
//!
//! ```c
//! if (!strcmp(arg, "--end-of-options")) {
//!         seen_end_of_options = 1;
//!         if (filter & (DO_FLAGS | DO_REVS))
//!                 show_file(arg, 0);
//!         continue;
//! }
//! ```
//! (builtin/rev-parse.c:1147-1152, v2.55.0). The flag it sets is read by
//! `if (!seen_end_of_options && *arg == '-')` (:795), which is the only thing
//! standing between a leading `-` and the revision parser — so after it, a
//! branch named `-tricky` can be named at all. It ends the *options* only: `--`
//! still works after it, and what follows is still read as revisions and paths.
//! The port rejected the option outright (`--end-of-options is not ported yet`,
//! exit 1), which is how git's own t1503 and t1506 spell it.
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
    /// One commit on `main`, with `refs/heads/-tricky` at it and a file `path`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-rp-eoo-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("path"), "a\n").unwrap();
        f.git(&["add", "path"]);
        f.git(&["commit", "-q", "-m", "subject"]);
        f.git(&["update-ref", "refs/heads/-tricky", "HEAD"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
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

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = self.cmd(args).output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn oid(&self, spec: &str) -> String {
        let (out, err, code) = self.run(&["rev-parse", spec]);
        assert_eq!((err.as_str(), code), ("", 0), "{spec}");
        out.trim().to_string()
    }
}

/// A branch whose name begins with `-` is reachable once the option parser has
/// been told to stop, and `--verify` swallows the marker itself.
#[test]
fn a_leading_dash_after_end_of_options_is_a_revision() {
    let f = Fixture::new("tricky");
    let head = f.oid("HEAD");
    let (out, err, code) = f.run(&["rev-parse", "--verify", "--end-of-options", "-tricky"]);
    assert_eq!((err.as_str(), code), ("", 0));
    assert_eq!(out, format!("{head}\n"));
}

/// Outside `--verify` the marker is echoed where it stood, like `--` is, and
/// `--` still works after it.
#[test]
fn the_marker_is_echoed_and_dashdash_still_follows() {
    let f = Fixture::new("echo");
    let head = f.oid("HEAD");
    let (out, err, code) = f.run(&["rev-parse", "--end-of-options", "HEAD", "--", "path"]);
    assert_eq!((err.as_str(), code), ("", 0));
    assert_eq!(out, format!("--end-of-options\n{head}\n--\npath\n"));
}

/// What follows is a revision, so a name that resolves to nothing is a bad
/// revision -- not an unknown option, and not a path.
#[test]
fn an_unresolvable_name_after_the_marker_is_a_bad_revision() {
    let f = Fixture::new("bad");
    let (_, err, code) = f.run(&["rev-parse", "--end-of-options", "--not-real", "--"]);
    assert_eq!(code, 128);
    assert!(err.contains("bad revision '--not-real'"), "{err:?}");
}

/// The marker stops being an option once it has been seen: a second one is just
/// another operand, and with a `--` in the vector that is a bad revision.
#[test]
fn a_second_marker_is_no_longer_an_option() {
    let f = Fixture::new("second");
    let (_, err, code) = f.run(&["rev-parse", "--end-of-options", "--end-of-options", "--"]);
    assert_eq!(code, 128);
    assert!(err.contains("bad revision '--end-of-options'"), "{err:?}");
}
