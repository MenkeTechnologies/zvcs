//! A `branch.<name>.merge` no configured refspec maps is fetched into `FETCH_HEAD` anyway.
//!
//! `get_ref_map()` reaches the configured refspecs when the remote has any *or* the current
//! branch's upstream is this remote (builtin/fetch.c:550-553), then `add_merge_config()`
//! fetches every merge value no earlier entry named through a source-only refspec with
//! `missing_ok`, marking it `FETCH_HEAD_MERGE` (:232-246, :570-572). `*autotags` is only set by a
//! configured refspec with a destination (:556-558). zvcs refused a remote without refspecs
//! ("Cannot perform a meaningful fetch operation…") and left an unmapped merge value out of
//! `FETCH_HEAD`, so `git pull` found no candidate.
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
    /// `work` clones `up` at `a` and merges `side`; `up` then gains `b` on `main` (tag `t2`)
    /// and `c` on `side` (tag `t3`).
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-fetch-merge-outside-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root, work };
        let up = f.root.join("up");
        f.run_in(&f.root, &["init", "-q", "-b", "main", "up"]);
        f.run_in(&up, &["commit", "-q", "--allow-empty", "-m", "a"]);
        f.run_in(&up, &["branch", "side"]);
        f.run_in(&f.root, &["clone", "-q", "up", "work"]);
        f.run_in(&f.work, &["config", "branch.main.merge", "refs/heads/side"]);
        f.run_in(&up, &["commit", "-q", "--allow-empty", "-m", "b"]);
        f.run_in(&up, &["tag", "t2"]);
        f.run_in(&up, &["checkout", "-q", "side"]);
        f.run_in(&up, &["commit", "-q", "--allow-empty", "-m", "c"]);
        f.run_in(&up, &["tag", "t3"]);
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

    fn work(&self, args: &[&str]) -> (String, String, i32) {
        self.run_in(&self.work, args)
    }

    fn up(&self, rev: &str) -> String {
        self.run_in(&self.root.join("up"), &["rev-parse", rev]).0.trim_end().to_owned()
    }

    fn fetch_head(&self) -> String {
        std::fs::read_to_string(self.work.join(".git/FETCH_HEAD")).unwrap()
    }

    fn url(&self) -> String {
        std::fs::canonicalize(self.root.join("up")).unwrap().display().to_string()
    }
}

#[test]
fn a_remote_without_refspecs_fetches_the_merge_value_and_no_tags() {
    let f = Fixture::new("none");
    f.work(&["config", "--unset-all", "remote.origin.fetch"]);
    let (out, err, code) = f.work(&["fetch"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", format!("From {}\n * branch            side       -> FETCH_HEAD\n", f.url()).as_str(), 0)
    );
    assert_eq!(f.fetch_head(), format!("{}\t\tbranch 'side' of {}\n", f.up("side"), f.url()));
    assert_eq!(f.work(&["tag", "-l"]).0, "");
}

#[test]
fn an_unmapped_merge_value_follows_the_mapped_rows_as_a_candidate() {
    let f = Fixture::new("mapped");
    f.work(&["config", "--replace-all", "remote.origin.fetch", "refs/heads/main:refs/remotes/origin/main"]);
    // A value naming nothing is no error, and one a configured refspec maps adds no second row.
    f.work(&["config", "--add", "branch.main.merge", "refs/heads/nope"]);
    f.work(&["config", "--add", "branch.main.merge", "main"]);
    assert_eq!(f.work(&["fetch", "-q"]).2, 0);
    let (main, side, url) = (f.up("main"), f.up("side"), f.url());
    assert_eq!(
        f.fetch_head(),
        format!(
            "{main}\t\tbranch 'main' of {url}\n\
             {side}\t\tbranch 'side' of {url}\n\
             {main}\tnot-for-merge\ttag 't2' of {url}\n\
             {side}\tnot-for-merge\ttag 't3' of {url}\n"
        )
    );
}

#[test]
fn refspecs_without_a_destination_follow_no_tags() {
    let f = Fixture::new("nodst");
    f.work(&["config", "--replace-all", "remote.origin.fetch", "refs/heads/main"]);
    f.work(&["config", "--unset", "branch.main.merge"]);
    assert_eq!(f.work(&["fetch", "-q"]).2, 0);
    assert_eq!(f.fetch_head(), format!("{}\t\tbranch 'main' of {}\n", f.up("main"), f.url()));
    assert_eq!(f.work(&["tag", "-l"]).0, "");
}
