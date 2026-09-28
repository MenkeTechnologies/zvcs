//! `git pull` after a fetch that marked nothing for merge.
//!
//! `die_no_merge_candidates()` (builtin/pull.c:315-366) picks one of five
//! explanations: refspecs were given but none matched; a remote other than
//! the branch's own was named without a branch; no branch; no merge
//! configuration; or a configured `branch.<name>.merge` the remote does not
//! have — each on stderr, exit 1. zvcs answered all but two of them with
//! `fatal: couldn't find remote ref <tracking ref>` at 128.
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
    /// `up` has `a` on `main`, `work` clones it, then `up` adds `b`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-pull-no-merge-candidates-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root, work };
        let up = f.root.join("up");
        f.run_in(&f.root, &["init", "-q", "-b", "main", "up"]);
        f.run_in(&up, &["commit", "-q", "--allow-empty", "-m", "a"]);
        f.run_in(&f.root, &["clone", "-q", "up", "work"]);
        f.run_in(&up, &["commit", "-q", "--allow-empty", "-m", "b"]);
        f
    }

    fn run_in(&self, dir: &Path, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
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
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        self.run_in(&self.work, args)
    }

}

fn stderr_tail(out: (String, String, i32)) -> (String, i32) {
    // Drop the fetch's `From`/update lines ahead of the explanation.
    let lines: Vec<&str> = out.1.lines().filter(|l| !l.starts_with("From ") && !l.starts_with("   ") && !l.starts_with(" * ")).collect();
    (lines.join("\n") + "\n", out.2)
}

#[test]
fn a_configured_merge_ref_the_remote_lacks() {
    let f = Fixture::new("zz");
    f.run(&["config", "branch.main.merge", "refs/heads/zz"]);
    assert_eq!(
        stderr_tail(f.run(&["pull"])),
        (
            "Your configuration specifies to merge with the ref 'refs/heads/zz'\n\
             from the remote, but no such ref was fetched.\n"
                .into(),
            1
        )
    );
}

#[test]
fn another_remote_without_a_branch() {
    let f = Fixture::new("other");
    f.run(&["remote", "add", "o2", "../up"]);
    assert_eq!(
        stderr_tail(f.run(&["pull", "o2"])),
        (
            "You asked to pull from the remote 'o2', but did not specify\n\
             a branch. Because this is not the default configured remote\n\
             for your current branch, you must specify a branch on the command line.\n"
                .into(),
            1
        )
    );
}

#[test]
fn a_wildcard_refspec_that_matched_nothing() {
    let f = Fixture::new("wildcard");
    let tail = "Generally this means that you provided a wildcard refspec which had no\n\
                matches on the remote end.\n";
    assert_eq!(
        f.run(&["pull", "origin", "refs/heads/nomatch*:refs/x/*"]),
        (
            String::new(),
            format!("There are no candidates for merging among the refs that you just fetched.\n{tail}"),
            1
        )
    );
    assert_eq!(
        f.run(&["pull", "--rebase", "origin", "refs/heads/nomatch*:refs/x/*"]),
        (
            String::new(),
            format!("There is no candidate for rebasing against among the refs that you just fetched.\n{tail}"),
            1
        )
    );
}
