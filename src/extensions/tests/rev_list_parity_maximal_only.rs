//! `rev-list --maximal-only` (git 2.55) was a usage error.
//!
//! - `get_commit_action()` ignores a commit carrying `CHILD_VISITED`
//!   (revision.c:4180), which `process_parents()` sets on each parent of a
//!   commit it processes, first parents only under `--first-parent`
//!   (revision.c:1150-1205). A streaming walk judges each commit as it pops, so
//!   a tip popped before its child — equal dates, first named — is still shown.
//! - `prepare_maximal_independent()` (builtin/rev-list.c:636-685) short-cuts the
//!   plain case: no exclusion and none of its listed modifiers (`--format`,
//!   `--parents`, `--first-parent`, ...) means the pending commits are replaced
//!   by `reduce_heads()` of them, whatever their dates.
//! - `--boundary` is refused (revision.c:3194-3195).
//! - `log` runs the same filter; it never takes the short cut, since it always
//!   sets `verbose_header`.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

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
    /// All at one timestamp: A, B on `main`; `side` forks at A with S; `main`
    /// then merges `side` as M.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-rev-list-maximal-only-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        for (file, msg) in [("a", "A"), ("b", "B")] {
            std::fs::write(f.work.join(file), format!("{file}\n")).unwrap();
            f.run(&["add", file]);
            f.run(&["commit", "-q", "-m", msg]);
        }
        f.run(&["checkout", "-q", "-b", "side", "main~1"]);
        std::fs::write(f.work.join("s"), "s\n").unwrap();
        f.run(&["add", "s"]);
        f.run(&["commit", "-q", "-m", "S"]);
        f.run(&["checkout", "-q", "main"]);
        f.run(&["merge", "-q", "--no-edit", "side", "-m", "M"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
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
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    /// The subjects `rev-list --format=%s` prints, without the `commit` lines.
    fn subjects(&self, args: &[&str]) -> String {
        let mut argv = vec!["rev-list", "--format=%s"];
        argv.extend_from_slice(args);
        let (out, err, code) = self.run(&argv);
        assert_eq!((err.as_str(), code), ("", 0), "{args:?}");
        out.lines().filter(|l| !l.starts_with("commit ")).map(|l| format!("{l}\n")).collect()
    }
}

#[test]
fn only_commits_no_other_walked_commit_reaches() {
    let f = Fixture::new("walk");
    let main = f.run(&["rev-parse", "main"]).0;
    // The plain case goes through `reduce_heads()`.
    assert_eq!(f.run(&["rev-list", "--maximal-only", "main~1", "main"]), (main.clone(), String::new(), 0));
    assert_eq!(f.run(&["rev-list", "--maximal-only", "--all"]), (main.clone(), String::new(), 0));
    // `--format` rules the short-cut out: B pops first, before M marks it.
    assert_eq!(f.subjects(&["--maximal-only", "main~1", "main"]), "B\nM\n");
    // Under `--first-parent` M never marks its second parent.
    assert_eq!(f.subjects(&["--maximal-only", "--first-parent", "main", "side"]), "M\nS\n");
    // An exclusion makes the walk limited: everything is marked before output.
    assert_eq!(f.subjects(&["--maximal-only", "main", "side", "^main~2"]), "M\n");
}

#[test]
fn boundary_is_refused() {
    let f = Fixture::new("boundary");
    let out = f.run(&["rev-list", "--maximal-only", "--boundary", "main"]);
    assert_eq!(
        out,
        (
            String::new(),
            "fatal: options '--boundary' and '--maximal-only' cannot be used together\n".to_string(),
            128
        )
    );
}

/// `git log` always sets `verbose_header`, so the `reduce_heads()` short cut is
/// never taken: the streaming walk shows B because it pops before M marks it.
#[test]
fn log_takes_the_walk_not_the_short_cut() {
    let f = Fixture::new("log");
    let out = f.run(&["log", "--format=%s", "--maximal-only", "main~1", "main"]);
    assert_eq!(out, ("B\nM\n".to_string(), String::new(), 0));
    let out = f.run(&["log", "--format=%s", "--maximal-only", "--all"]);
    assert_eq!(out, ("M\n".to_string(), String::new(), 0));
    let out = f.run(&["log", "--format=%s", "--maximal-only", "--first-parent", "main", "side"]);
    assert_eq!(out, ("M\nS\n".to_string(), String::new(), 0));
    let out = f.run(&["log", "--maximal-only", "--boundary", "main"]);
    assert_eq!(
        out,
        (
            String::new(),
            "fatal: options '--boundary' and '--maximal-only' cannot be used together\n".to_string(),
            128
        )
    );
}
