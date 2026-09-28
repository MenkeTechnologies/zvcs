//! Every `branch.<name>.merge` names a `FETCH_HEAD` merge candidate.
//!
//! `add_merge_config()` (builtin/fetch.c:212-248) loops over the current
//! branch's merge values (`branch->merge_nr`) and, for each, marks the first
//! ref-map entry `branch_merge_matches()` — `refname_match()`, so a short
//! value like `main` counts — as `FETCH_HEAD_MERGE`. zvcs took one merge
//! value (the last), so a branch merging two heads had one candidate, and the
//! wrong one first in `FETCH_HEAD`.
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
    /// `work` clones `up`; `up` then gains a commit and a branch `side`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-fetch-every-merge-value-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root, work };
        let up = f.root.join("up");
        f.run_in(&f.root, &["init", "-q", "-b", "main", "up"]);
        f.run_in(&up, &["commit", "-q", "--allow-empty", "-m", "a"]);
        f.run_in(&f.root, &["clone", "-q", "up", "work"]);
        f.run_in(&up, &["commit", "-q", "--allow-empty", "-m", "b"]);
        f.run_in(&up, &["branch", "side"]);
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
}

fn fetch_head(f: &Fixture) -> String {
    std::fs::read_to_string(f.work.join(".git/FETCH_HEAD")).unwrap()
}

fn main_id(f: &Fixture) -> String {
    f.run_in(&f.root.join("up"), &["rev-parse", "main"]).0.trim_end().to_owned()
}

#[test]
fn two_merge_values_mark_two_candidates() {
    let f = Fixture::new("two");
    f.run_in(&f.work, &["config", "--add", "branch.main.merge", "refs/heads/side"]);
    assert_eq!(f.run_in(&f.work, &["fetch", "-q"]).2, 0);
    let id = main_id(&f);
    assert_eq!(
        fetch_head(&f),
        format!("{id}\t\tbranch 'main' of {0}\n{id}\t\tbranch 'side' of {0}\n", url(&f))
    );
}

#[test]
fn a_short_merge_value_still_matches() {
    let f = Fixture::new("short");
    f.run_in(&f.work, &["config", "branch.main.merge", "main"]);
    assert_eq!(f.run_in(&f.work, &["fetch", "-q"]).2, 0);
    let id = main_id(&f);
    assert_eq!(
        fetch_head(&f),
        format!("{id}\t\tbranch 'main' of {0}\n{id}\tnot-for-merge\tbranch 'side' of {0}\n", url(&f))
    );
}

fn url(f: &Fixture) -> String {
    std::fs::canonicalize(f.root.join("up")).unwrap().display().to_string()
}
