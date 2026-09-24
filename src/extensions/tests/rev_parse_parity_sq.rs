//! `git rev-parse --sq`.
//!
//! ```c
//! static void show(const char *arg)
//! {
//!         if (output_sq) {
//!                 int sq = '\'', ch;
//!
//!                 putchar(sq);
//!                 while ((ch = *arg++)) {
//!                         if (ch == sq)
//!                                 fputs("'\\'", stdout);
//!                         putchar(ch);
//!                 }
//!                 putchar(sq);
//!                 putchar(' ');
//!         }
//!         else
//!                 puts(arg);
//! }
//! ```
//! (builtin/rev-parse.c:117-133, v2.55.0). Every revision, echoed flag and path
//! goes through `show()`; the `^` of an excluded revision is `putchar('^')`
//! ahead of it (`show_with_type()`, :135-140), and the query options `puts()`
//! directly, so they keep their own lines. The run never ends in a newline. The
//! port rejected the option (`--sq is not ported yet`, exit 1).
//!
//! Every expectation was measured from stock git 2.55.0 in an identical
//! throwaway repository.
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
    /// Two commits on `main` touching `path`, and a branch `it's` at the first.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-rp-sq-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("path"), "a\n").unwrap();
        f.git(&["add", "path"]);
        f.git(&["commit", "-q", "-m", "one"]);
        std::fs::write(f.work.join("path"), "a\nb\n").unwrap();
        f.git(&["commit", "-q", "-a", "-m", "two"]);
        f.git(&["branch", "it's", "HEAD~1"]);
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

}

const ONE: &str = "3d45988a7d5b295f1de59a48e10c279d4234391d";
const TWO: &str = "4b524e80d237a9a4e78b766934d82600038bc173";

/// Revisions, the `^` of a range's bottom, an echoed flag, `--` and a path all
/// land on one line, each quoted and followed by a space, with no newline.
#[test]
fn every_shown_value_is_quoted_onto_one_line() {
    let f = Fixture::new("line");
    let (out, err, code) = f.run(&["rev-parse", "--sq", "HEAD~1..HEAD", "it's", "--foo", "--", "path"]);
    assert_eq!((err.as_str(), code), ("", 0));
    assert_eq!(out, format!("'{TWO}' ^'{ONE}' '{ONE}' '--foo' '--' 'path' "));
}

/// A `'` inside a value is closed, escaped and reopened; `--symbolic` names go
/// through the same quoting, the `^` still outside it.
#[test]
fn a_quote_in_a_name_is_escaped() {
    let f = Fixture::new("quote");
    let (out, err, code) = f.run(&["rev-parse", "--sq", "--symbolic", "it's^!"]);
    assert_eq!((err.as_str(), code), ("", 0));
    assert_eq!(out, "'it'\\''s' ");
}

/// `--git-dir` is a `puts()`, not a `show()`: its line stays unquoted and
/// newline-terminated in the middle of a quoted run.
#[test]
fn query_options_are_not_quoted() {
    let f = Fixture::new("query");
    let (out, err, code) = f.run(&["rev-parse", "--sq", "--git-dir", "HEAD"]);
    assert_eq!((err.as_str(), code), ("", 0));
    assert_eq!(out, format!(".git\n'{TWO}' "));
}
