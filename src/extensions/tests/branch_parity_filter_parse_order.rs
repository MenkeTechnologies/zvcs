//! `--contains`/`--no-contains` (`parse_opt_commits()`, parse-options-cb.c:89-105)
//! and `--points-at` (`parse_opt_object_name()`, parse-options-cb.c:126-140)
//! are option callbacks: each resolves its operand while argv is walked, and a
//! failure is `PARSE_OPT_ERROR`, exit 129, before any later option is parsed.
//! So the first bad operand on the command line is the one reported, ahead of
//! a later `--merged` operand, an unknown option, or the action tally.
//!
//! Every expectation was measured from stock git in an identical throwaway
//! repository under the same pinned environment.
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
    /// Two commits, so two tags can point at different objects.
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("zvcs-br-filterorder-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "one\n").unwrap();
        f.git(&["add", "a"]);
        f.git(&["commit", "-q", "-m", "one"]);
        std::fs::write(f.work.join("a"), "two\n").unwrap();
        f.git(&["commit", "-q", "-am", "two"]);
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

    fn stdout(&self, args: &[&str]) -> String {
        let (out, err, code) = self.run(args);
        assert_eq!((err.as_str(), code), ("", 0), "`git {args:?}`");
        out
    }
}

/// A bad `--contains`/`--points-at` operand ahead of a bad `--merged` one is
/// reported as the callback's error at 129; the other way round, `--merged`'s
/// `die()` comes first at 128.
#[test]
fn the_first_bad_filter_operand_on_the_command_line_is_reported() {
    let f = Fixture::new("order");

    let cases: [(&[&str], &str, i32); 4] = [
        (
            &["branch", "--points-at", "nosuch", "--merged", "nosuch2"],
            "error: malformed object name 'nosuch'\n",
            129,
        ),
        (
            &["branch", "--contains", "nosuch", "--merged", "nosuch2"],
            "error: malformed object name nosuch\n",
            129,
        ),
        (
            &["branch", "--without", "nosuch", "--no-merged", "nosuch2"],
            "error: malformed object name nosuch\n",
            129,
        ),
        (
            &["branch", "--merged", "nosuch2", "--contains", "nosuch"],
            "fatal: malformed object name nosuch2\n",
            128,
        ),
    ];
    for (args, err, code) in cases {
        let (out, e, c) = f.run(args);
        assert_eq!((out.as_str(), e.as_str(), c), ("", err, code), "{args:?}");
    }
}

/// The operand is refused before a later unknown option or a second action
/// would print the usage block.
#[test]
fn a_bad_contains_operand_outranks_later_usage_errors() {
    let f = Fixture::new("usage");

    for args in [
        &["branch", "--contains", "nosuch", "--bogus"][..],
        &["branch", "--no-contains", "nosuch", "-d", "foo"][..],
    ] {
        let (out, err, code) = f.run(args);
        assert_eq!(
            (out.as_str(), err.as_str(), code),
            ("", "error: malformed object name nosuch\n", 129),
            "{args:?}"
        );
    }
}

/// Resolved filters still select the same branches.
#[test]
fn resolved_filters_still_select_branches() {
    let f = Fixture::new("select");
    f.git(&["branch", "side", "HEAD~"]);

    assert_eq!(f.stdout(&["branch", "--with", "HEAD~"]), "* main\n  side\n");
    assert_eq!(f.stdout(&["branch", "--without", "HEAD"]), "  side\n");
    assert_eq!(f.stdout(&["branch", "--points-at", "HEAD~"]), "  side\n");
    assert_eq!(
        f.stdout(&["branch", "--points-at", "HEAD~", "--no-points-at"]),
        "* main\n  side\n"
    );
}
