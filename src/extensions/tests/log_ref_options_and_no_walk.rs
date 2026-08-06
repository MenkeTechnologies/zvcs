//! The revision-naming options an IDE drives `log` with, and the walk that used to
//! never end.
//!
//! `--branches`/`--tags`/`--remotes` are `--all` restricted to one namespace, and
//! they take their tips at the position they were written at — `setup_revisions()`
//! appends to a single pending list as it reads the command line, so `--all HEAD`
//! and `HEAD --all` order their tips differently. `--not` reverses the sense of the
//! revisions after it, `--no-walk` shows the named commits and traverses nothing,
//! and `rev_input_given` means a command line that named only *excluded* revisions
//! walks nothing rather than falling back to `HEAD`.
//!
//! `blame` is here because the same fixture proves it: a history that merges reaches
//! its root commit while other commits are still queued, and the walk used to keep
//! re-selecting that root forever.
//!
//! Expectations measured against stock git.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// `master` and `feature` diverge, `master` renames the file and merges
    /// `feature` back, and a tag sits on the root commit.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-logrefs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let f = Fixture { root, repo };
        f.git(&["init", "-q", "-b", "master", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f.write("a.txt", "one\ntwo\nthree\n");
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "seed"]);
        f.git(&["tag", "v1"]);
        f.git(&["checkout", "-q", "-b", "feature"]);
        f.write("a.txt", "one\ntwo\nthree\nfour\n");
        f.git(&["commit", "-qam", "on feature"]);
        f.git(&["checkout", "-q", "master"]);
        f.git(&["mv", "a.txt", "renamed.txt"]);
        f.git(&["commit", "-qam", "rename a"]);
        f.git(&["merge", "-q", "--no-ff", "-m", "merge feature", "feature"]);
        f
    }

    fn write(&self, path: &str, body: &str) {
        std::fs::write(self.repo.join(path), body).unwrap();
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.repo)
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

    fn run(&self, args: &[&str]) -> std::process::Output {
        self.cmd(args).output().unwrap()
    }

    /// The subjects `git log --format=%s <args>` prints, in order.
    fn subjects(&self, args: &[&str]) -> Vec<String> {
        let mut a = vec!["log", "--format=%s"];
        a.extend_from_slice(args);
        let out = self.run(&a);
        assert_eq!(out.status.code(), Some(0), "`git {a:?}`: {out:?}");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

/// The namespace forms of `--all`, including their `=<glob>` filter.
#[test]
fn ref_namespaces_name_their_own_tips() {
    let f = Fixture::new("ns");

    // `--branches` reaches both branch tips; `--tags` reaches only the tagged root.
    assert_eq!(
        f.subjects(&["--branches"]),
        ["on feature", "merge feature", "seed", "rename a"]
    );
    assert_eq!(f.subjects(&["--tags"]), ["seed"]);
    // No remote-tracking refs exist — and that is still a revision *input*, so the
    // walk does not quietly fall back to HEAD.
    assert!(f.subjects(&["--remotes"]).is_empty());

    // `=<glob>` matches the shortened name.
    assert_eq!(f.subjects(&["--branches=fea*"]), ["on feature", "seed"]);
    assert!(f.subjects(&["--branches=nope*"]).is_empty());
}

/// A pseudo-option takes its tips where it stands, which decides a date tie.
#[test]
fn tips_are_collected_in_command_line_order() {
    let f = Fixture::new("order");
    // Every commit here shares a timestamp, so the order is entirely the order the
    // tips were pended in: refs first when `--all` leads, `HEAD` first when it does.
    let refs_first = f.subjects(&["--all", "HEAD"]);
    let head_first = f.subjects(&["HEAD", "--all"]);
    assert_eq!(refs_first[0], "on feature");
    assert_eq!(head_first[0], "merge feature");
    let mut a = refs_first.clone();
    let mut b = head_first.clone();
    a.sort();
    b.sort();
    assert_eq!(a, b, "the same commits either way, only the order differs");
}

/// `--not` reverses the sense of what follows, and toggles again at the next one.
#[test]
fn not_reverses_the_revisions_after_it() {
    let f = Fixture::new("not");

    assert_eq!(
        f.subjects(&["master", "--not", "feature"]),
        ["merge feature", "rename a"]
    );
    // A second `--not` puts the sense back, so `master` is the kept side again.
    assert_eq!(
        f.subjects(&["--not", "feature", "--not", "master"]),
        ["merge feature", "rename a"]
    );
    // Only excluded revisions: nothing is kept, and `HEAD` is not substituted.
    assert!(f.subjects(&["--not", "feature"]).is_empty());
    assert!(f.subjects(&["^feature"]).is_empty());
    // A bare `git log` still defaults to HEAD.
    assert_eq!(f.subjects(&[])[0], "merge feature");
}

/// `--no-walk` shows the named commits and traverses nothing.
#[test]
fn no_walk_shows_only_the_named_commits() {
    let f = Fixture::new("nowalk");

    let sorted = f.subjects(&["--no-walk", "HEAD", "feature"]);
    assert_eq!(sorted.len(), 2, "no traversal: {sorted:?}");
    assert!(sorted.contains(&"merge feature".to_owned()));
    assert!(sorted.contains(&"on feature".to_owned()));

    // `unsorted` keeps the order the commits were named in; `sorted` (the default)
    // does not have to.
    assert_eq!(
        f.subjects(&["--no-walk=unsorted", "feature", "HEAD"]),
        ["on feature", "merge feature"]
    );

    // `--do-walk` puts the traversal back.
    assert!(f.subjects(&["--do-walk", "HEAD"]).len() > 2);
}

/// `--timestamp` and `--no-stat` belong to other commands; git refuses them here.
#[test]
fn log_refuses_options_it_does_not_own() {
    let f = Fixture::new("refuse");
    for flag in ["--timestamp", "--no-stat"] {
        let out = f.run(&["log", flag, "--format=%H"]);
        assert_eq!(out.status.code(), Some(128), "{flag}: {out:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            format!("fatal: unrecognized argument: {flag}\n")
        );
    }
    // `rev-list` is where `--timestamp` lives, and it prefixes the commit date.
    let out = f.run(&["rev-list", "--timestamp", "--max-count=1", "HEAD"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let line = String::from_utf8_lossy(&out.stdout).trim_end().to_owned();
    let (secs, id) = line.split_once(' ').expect("`<seconds> <id>`: {line}");
    assert!(secs.parse::<i64>().is_ok(), "{line}");
    assert_eq!(id.len(), 40, "{line}");
}

/// A history that merges reaches its root with commits still queued, which used to
/// leave `blame` re-selecting that root forever.
#[test]
fn blame_terminates_on_a_merged_history() {
    let f = Fixture::new("blame");
    let out = f.run(&["blame", "--porcelain", "renamed.txt"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    // All four lines are accounted for — porcelain writes each one as a `\t`-prefixed
    // body line — and the ones the root commit is responsible for name the file by
    // the path it had before the rename.
    let body: Vec<&str> = text
        .lines()
        .filter_map(|l| l.strip_prefix('\t'))
        .collect();
    assert_eq!(body, ["one", "two", "three", "four"], "{text}");
    // Both suspects trace through the rename, so every line is credited to the file's
    // former path rather than the one it is blamed under.
    assert!(text.contains("filename a.txt"), "{text}");
    assert!(!text.contains("filename renamed.txt"), "{text}");
}
