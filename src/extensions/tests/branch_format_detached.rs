//! `git branch --format` and the detached-`HEAD` pseudo entry.
//!
//! `filter_refs()` adds the detached `HEAD` as a `ref_array_item` like any other, so it
//! is rendered by the user's format rather than printed verbatim: a format with no atoms
//! prints its literal text for that line too. Only the name is substituted —
//! `get_head_description()` stands in for a ref name it does not have, which is what
//! `%(refname)` expands to.
//!
//! Measured against stock git 2.55.0.
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
    /// One commit on `main`, an idle `other`, and a detached `HEAD`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-branchfmt-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        std::fs::write(f.work.join("f.txt"), b"a\n").unwrap();
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "one"]);
        f.git(&["branch", "other"]);
        f.git(&["checkout", "-q", "--detach", "HEAD"]);
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

    /// stdout of a successful run, split on newlines with the trailing one dropped.
    fn lines(&self, args: &[&str]) -> Vec<String> {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let text = text.strip_suffix('\n').unwrap_or(&text).to_string();
        if text.is_empty() && out.stdout.is_empty() {
            return Vec::new();
        }
        text.split('\n').map(str::to_string).collect()
    }

    /// The `(HEAD detached at <abbrev>)` line the plain listing prints, minus its
    /// `* ` marker. Taken from the listing rather than rebuilt, because the width
    /// `get_head_description()` abbreviates to is `DEFAULT_ABBREV`, not the wider
    /// auto-scaled one `rev-parse --short` picks.
    fn head_description(&self) -> String {
        let first = self.lines(&["branch", "--list"]).remove(0);
        let described = first.strip_prefix("* ").expect("the detached entry is current");
        assert!(
            described.starts_with("(HEAD detached at "),
            "unexpected first entry: {described}"
        );
        described.to_string()
    }
}

/// A format made only of literal text renders that text for the detached entry too —
/// three refs in, three `0`s out.
#[test]
fn a_literal_format_prints_for_the_detached_entry() {
    let f = Fixture::new("literal");
    assert_eq!(f.lines(&["branch", "-a", "--list", "--format=0"]), ["0", "0", "0"]);
}

/// An empty format yields an empty line per entry, the detached one included.
#[test]
fn an_empty_format_yields_one_empty_line_per_entry() {
    let f = Fixture::new("empty");
    assert_eq!(f.lines(&["branch", "-a", "--list", "--format="]), ["", "", ""]);
    assert!(
        f.lines(&["branch", "-a", "--list", "--format=", "--omit-empty"]).is_empty(),
        "--omit-empty drops every line that rendered to nothing, the detached one too"
    );
}

/// `%(refname)` has no ref name to expand for the detached entry, so
/// `get_head_description()` stands in — `%(refname:short)` gives the same text.
#[test]
fn refname_expands_to_the_head_description() {
    let f = Fixture::new("refname");
    let described = f.head_description();
    assert_eq!(
        f.lines(&["branch", "-a", "--list", "--format=%(refname)"]),
        [described.as_str(), "refs/heads/main", "refs/heads/other"]
    );
    assert_eq!(
        f.lines(&["branch", "-a", "--list", "--format=%(refname:short)"])[0],
        described
    );
    assert_eq!(
        f.lines(&["branch", "-a", "--list", "--format=[%(refname)]"])[0],
        format!("[{described}]"),
        "the substitution happens inside the format, not instead of it"
    );
}

/// The rest of the atoms treat the detached entry as the ordinary item it is: it is the
/// checked-out one, so `%(HEAD)` marks it, and it carries a real object id.
#[test]
fn the_other_atoms_treat_the_detached_entry_normally() {
    let f = Fixture::new("atoms");
    let lines = f.lines(&["branch", "-a", "--list", "--format=%(HEAD)|%(objectname:short)"]);
    let tip = {
        let out = f.cmd(&["rev-parse", "--short", "HEAD"]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    assert_eq!(lines[0], format!("*|{tip}"), "the detached entry is the current one");
    assert_eq!(lines[1], format!(" |{tip}"));
    assert_eq!(lines[2], format!(" |{tip}"));
}
