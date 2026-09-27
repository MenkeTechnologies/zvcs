//! `git merge` runs automatic maintenance when it moves `HEAD`.
//!
//! `finish()` (builtin/merge.c:480-515) updates `HEAD` and then calls
//! `run_auto_maintenance(the_repository, verbosity < 0)` — for a fast-forward
//! and for a merge commit alike, and not for `--squash` or a merge stopped by
//! `--no-commit`, which never move `HEAD`. zvcs never made the call.
//!
//! The commit-graph task makes it visible, as in
//! `commit_parity_auto_maintenance.rs`.
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
        let root = std::env::temp_dir().join(format!("zvcs-merge-auto-maintenance-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["-c", "maintenance.auto=false", "commit", "-q", "-m", "i"]);
        f.run(&["checkout", "-q", "-b", "side"]);
        std::fs::write(f.work.join("b"), "b\n").unwrap();
        f.run(&["add", "b"]);
        f.run(&["-c", "maintenance.auto=false", "commit", "-q", "-m", "side"]);
        f.run(&["checkout", "-q", "main"]);
        for (key, value) in [
            ("maintenance.autoDetach", "false"),
            ("maintenance.commit-graph.enabled", "true"),
            ("maintenance.commit-graph.auto", "1"),
        ] {
            f.run(&["config", key, value]);
        }
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
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@example.com")
            .env("GIT_COMMITTER_NAME", "C")
            .env("GIT_COMMITTER_EMAIL", "c@example.com")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn graph_written(&self) -> bool {
        self.work.join(".git/objects/info/commit-graphs").exists()
    }
}

#[test]
fn a_fast_forward_and_a_merge_commit_run_it() {
    let f = Fixture::new("runs");
    assert_eq!(f.run(&["merge", "-q", "side"]), (String::new(), String::new(), 0));
    assert!(f.graph_written());
    std::fs::remove_dir_all(f.work.join(".git/objects/info/commit-graphs")).unwrap();
    f.run(&["-c", "maintenance.auto=false", "reset", "-q", "--hard", "HEAD~1"]);
    assert!(!f.graph_written());
    assert_eq!(f.run(&["merge", "-q", "--no-ff", "-m", "m", "side"]), (String::new(), String::new(), 0));
    assert!(f.graph_written());
}

#[test]
fn a_merge_that_leaves_head_alone_does_not() {
    let f = Fixture::new("still");
    assert_eq!(f.run(&["merge", "-q", "--squash", "side"]).2, 0);
    assert!(!f.graph_written());
    f.run(&["reset", "-q", "--hard"]);
    assert_eq!(f.run(&["merge", "-q", "--no-ff", "--no-commit", "side"]).2, 0);
    assert!(!f.graph_written());
}
