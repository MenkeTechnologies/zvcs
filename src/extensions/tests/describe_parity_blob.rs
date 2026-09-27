//! `git describe <blob>` named the wrong commit and the wrong path.
//!
//! `describe_blob()` (builtin/describe.c:566-597) runs
//! `--objects --in-commit-order --reverse HEAD` and stops at the first
//! `process_object()` call for the blob. Two things decide the answer:
//!
//! - the walk is git's default commit-date order, reversed — zvcs took
//!   gitoxide's breadth-first order, so a commit close to `HEAD` in the graph
//!   beat an older one on the other side of a merge;
//! - `process_tree()` (list-objects.c) enters a subtree the moment it meets
//!   it, so a blob at both `d/f` and `d0` is reported as `d/f` (`d` sorts
//!   before `d0`) — zvcs scanned breadth-first and said `d0`.
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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-describe-blob-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."], 0);
        f
    }

    fn write(&self, path: &str, body: &str) {
        let path = self.work.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
        self.run(&["add", "."], 0);
    }

    /// Commit, and optionally tag, at `at` seconds past the base date.
    fn commit(&self, at: u64, msg: &str, tag: Option<&str>) {
        self.run(&["commit", "-q", "-m", msg], at);
        if let Some(tag) = tag {
            self.run(&["tag", "-a", "-m", tag, tag], at);
        }
    }

    fn run(&self, args: &[&str], at: u64) -> (String, String, i32) {
        let date = format!("@{} +0000", 1_700_000_000 + at);
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
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
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
}

/// `side`'s X1 (older) and `main`'s M1 both add the same `f`; M2 sits between
/// M1 and the merge, so breadth-first reaches X1 before M1 while date order
/// puts X1 first once the walk is reversed.
#[test]
fn the_oldest_commit_in_date_order_names_the_blob() {
    let f = Fixture::new("order");
    f.write("a", "a\n");
    f.commit(100, "A", None);
    f.run(&["checkout", "-q", "-b", "side"], 0);
    f.write("f", "f\n");
    f.commit(120, "X1", Some("vx"));
    f.run(&["checkout", "-q", "main"], 0);
    f.write("f", "f\n");
    f.commit(150, "M1", Some("vm"));
    f.write("m", "m\n");
    f.commit(500, "M2", None);
    f.run(&["merge", "-q", "--no-edit", "side", "-m", "M"], 600);
    assert_eq!(f.run(&["describe", "HEAD:f"], 0), ("vx:f\n".into(), String::new(), 0));
}

#[test]
fn a_subtree_is_searched_before_the_entries_after_it() {
    let f = Fixture::new("path");
    f.write("d/f", "g\n");
    f.write("d0", "g\n");
    f.commit(100, "R", Some("vr"));
    f.write("y", "y\n");
    f.commit(200, "S", None);
    assert_eq!(f.run(&["describe", "HEAD:d0"], 0), ("vr:d/f\n".into(), String::new(), 0));
}
