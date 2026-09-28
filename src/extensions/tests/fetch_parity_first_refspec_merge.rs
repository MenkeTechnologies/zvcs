//! `FETCH_HEAD`'s merge candidate from the first configured refspec.
//!
//! With no refspec on the command line and no merge configuration for the
//! current branch (`branch_has_merge_config()`: both `branch.<name>.remote` and
//! `.merge`), `get_ref_map()` marks the ref the first configured refspec
//! names exactly as `FETCH_HEAD_MERGE` (builtin/fetch.c:559-561); a pattern
//! refspec marks nothing. zvcs only ever picked the upstream's ref, so such a
//! fetch left `FETCH_HEAD` without a merge candidate for `git pull`.
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
    /// `up` has `main` and `topic`; `work` is a fresh repository with a remote
    /// `x` whose refspecs name `topic`, then `main`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-fetch-first-refspec-merge-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root, work };
        let up = f.root.join("up");
        f.run_in(&f.root, &["init", "-q", "-b", "main", "up"]);
        f.run_in(&up, &["commit", "-q", "--allow-empty", "-m", "a"]);
        f.run_in(&up, &["branch", "topic"]);
        f.run_in(&up, &["commit", "-q", "--allow-empty", "-m", "b"]);
        f.run_in(&f.root, &["init", "-q", "-b", "main", "work"]);
        f.run(&["config", "remote.x.url", "../up"]);
        f.run(&["config", "remote.x.fetch", "+refs/heads/topic:refs/remotes/x/topic"]);
        f.run(&["config", "--add", "remote.x.fetch", "+refs/heads/main:refs/remotes/x/main"]);
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

    fn rev(&self, rev: &str) -> String {
        self.run_in(&self.root.join("up"), &["rev-parse", rev]).0.trim_end().to_owned()
    }
}

fn fetch_head(f: &Fixture) -> String {
    std::fs::read_to_string(f.work.join(".git/FETCH_HEAD")).unwrap()
}

#[test]
fn the_first_exact_refspec_names_the_merge_candidate() {
    let f = Fixture::new("exact");
    assert_eq!(f.run(&["fetch", "-q", "x"]).2, 0);
    assert_eq!(
        fetch_head(&f),
        format!(
            "{}\t\tbranch 'topic' of ../up\n{}\tnot-for-merge\tbranch 'main' of ../up\n",
            f.rev("topic"),
            f.rev("main")
        )
    );
}

#[test]
fn an_upstream_or_a_pattern_takes_the_rule_away() {
    let f = Fixture::new("upstream");
    f.run(&["config", "branch.main.remote", "x"]);
    f.run(&["config", "branch.main.merge", "refs/heads/nope"]);
    assert_eq!(f.run(&["fetch", "-q", "x"]).2, 0);
    assert!(!fetch_head(&f).contains("\t\t"), "{}", fetch_head(&f));

    let f = Fixture::new("pattern");
    f.run(&["config", "--replace-all", "remote.x.fetch", "+refs/heads/*:refs/remotes/x/*"]);
    assert_eq!(f.run(&["fetch", "-q", "x"]).2, 0);
    assert!(!fetch_head(&f).contains("\t\t"), "{}", fetch_head(&f));
}
