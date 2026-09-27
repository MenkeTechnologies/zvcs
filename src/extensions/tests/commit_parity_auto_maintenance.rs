//! `git commit` ends with automatic maintenance.
//!
//! `cmd_commit()` calls `run_auto_maintenance(the_repository, quiet)` right
//! after `repo_rerere()` (builtin/commit.c:1964-1965), which runs
//! `git maintenance run --auto` unless `maintenance.auto` (or, unset, a
//! non-positive `gc.auto`) turns it off. zvcs never made the call, so a task
//! whose auto condition held after a commit never ran.
//!
//! The commit-graph task makes it visible: enabled, with
//! `maintenance.commit-graph.auto = 1`, one commit the graph does not carry is
//! enough for `--auto` to write `objects/info/commit-graphs`.
//! `maintenance.autoDetach = false` makes the commit wait for it.
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
        let root = std::env::temp_dir().join(format!("zvcs-commit-auto-maintenance-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["-c", "maintenance.auto=false", "commit", "-q", "-m", "i"]);
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
fn a_commit_runs_auto_maintenance() {
    let f = Fixture::new("runs");
    assert!(!f.graph_written());
    assert_eq!(f.run(&["commit", "-q", "--allow-empty", "-m", "k"]), (String::new(), String::new(), 0));
    assert!(f.graph_written());
}

#[test]
fn maintenance_auto_false_or_gc_auto_zero_turns_it_off() {
    let f = Fixture::new("off");
    assert_eq!(f.run(&["-c", "maintenance.auto=false", "commit", "-q", "--allow-empty", "-m", "k"]).2, 0);
    assert!(!f.graph_written());
    assert_eq!(f.run(&["-c", "gc.auto=0", "commit", "-q", "--allow-empty", "-m", "l"]).2, 0);
    assert!(!f.graph_written());
    // `maintenance.auto` answers before `gc.auto` is consulted.
    assert_eq!(
        f.run(&["-c", "gc.auto=0", "-c", "maintenance.auto=true", "commit", "-q", "--allow-empty", "-m", "m"]).2,
        0
    );
    assert!(f.graph_written());
}
