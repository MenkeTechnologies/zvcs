//! Clustered short options on `git pull`.
//!
//! `parse_short_opt()` reads `-nq` one switch at a time; `-s`, `-X` and `-o`
//! take the rest of their word as the value, and `-r`, `-S` and `-j` carry
//! `PARSE_OPT_OPTARG`, so `-rmerges` is `--rebase=merges` and `-j2` is
//! `--jobs=2` (parse-options.c:47-62, 426-461; builtin/pull.c:871-1000). An
//! unknown character is named alone. zvcs split nothing: `-nq` was ``unknown
//! switch `n'``, `-j2` was refused and `-vx` blamed `v`.
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
            .join(format!("zvcs-pull-short-clusters-{tag}-{}", std::process::id()));
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

    fn rev(&self, dir: &str, rev: &str) -> String {
        self.run_in(&self.root.join(dir), &["rev-parse", rev]).0
    }
}

#[test]
fn a_cluster_of_switches_is_each_switch() {
    let f = Fixture::new("flags");
    assert_eq!(f.run(&["pull", "-nq"]), (String::new(), String::new(), 0));
    assert_eq!(f.rev("work", "HEAD"), f.rev("up", "main"));
}

#[test]
fn optional_values_stay_attached() {
    let f = Fixture::new("optargs");
    assert_eq!(f.run(&["pull", "-qj2"]), (String::new(), String::new(), 0));
    assert_eq!(f.rev("work", "HEAD"), f.rev("up", "main"));
    assert_eq!(
        f.run(&["pull", "-rbogus"]),
        (String::new(), "error: invalid value for '--rebase': 'bogus'\n".into(), 129)
    );
}

#[test]
fn an_unknown_character_is_named_alone() {
    let f = Fixture::new("unknown");
    let (out, err, code) = f.run(&["pull", "-vx"]);
    assert_eq!((out.as_str(), code), ("", 129));
    assert!(err.starts_with("error: unknown switch `x'\nusage: git pull "), "{err}");
}
