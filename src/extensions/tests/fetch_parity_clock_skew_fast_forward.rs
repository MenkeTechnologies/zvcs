//! A fast-forward whose new commit is dated before the old one.
//!
//! `update_local_ref()` decides fast-forward with
//! `repo_in_merge_bases(the_repository, current, updated)`
//! (builtin/fetch.c:1049-1050), an exact reachability test. zvcs walked the
//! new tip's ancestry cut off at the old commit's committer date, so a child
//! committed with an earlier date (clock skew) was "not a descendant": a
//! `+` refspec reported `(forced update)` and a plain one rejected the update
//! as non-fast-forward.
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
    /// `up` gets a commit dated 1700005000, `work` clones it (and copies it to
    /// branch `copy`), then `up` gets a child dated 1700000000.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-fetch-clock-skew-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root, work };
        f.run_in(&f.root, "1700005000", &["init", "-q", "-b", "main", "up"]);
        f.run_in(&f.root.join("up"), "1700005000", &["commit", "-q", "--allow-empty", "-m", "a"]);
        f.run_in(&f.root, "1700005000", &["clone", "-q", "up", "work"]);
        f.run(&["branch", "copy", "origin/main"]);
        f.run_in(&f.root.join("up"), "1700000000", &["commit", "-q", "--allow-empty", "-m", "b"]);
        f
    }

    fn run_in(&self, dir: &std::path::Path, date: &str, args: &[&str]) -> (String, String, i32) {
        let date = format!("{date} +0000");
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
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
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
        self.run_in(&self.work, "1700005000", args)
    }

    fn short(&self, rev: &str) -> String {
        self.run(&["rev-parse", "--short", rev]).0.trim_end().to_owned()
    }
}

#[test]
fn a_forced_refspec_reports_a_plain_fast_forward() {
    let f = Fixture::new("forced");
    let old = f.short("origin/main");
    let (out, err, code) = f.run(&["fetch"]);
    let new = f.short("origin/main");
    assert_eq!(
        (out.as_str(), code),
        ("", 0)
    );
    assert!(err.ends_with(&format!("   {old}..{new}  main       -> origin/main\n")), "{err}");
    let reflog = f.run(&["reflog", "-1", "--format=%gs", "refs/remotes/origin/main"]).0;
    assert_eq!(reflog, "fetch: fast-forward\n");
}

#[test]
fn a_plain_refspec_is_not_rejected() {
    let f = Fixture::new("plain");
    let old = f.short("copy");
    let (out, err, code) = f.run(&["fetch", "origin", "main:copy"]);
    let new = f.short("copy");
    assert_ne!(old, new);
    assert_eq!((out.as_str(), code), ("", 0));
    assert!(err.contains(&format!("   {old}..{new}  main       -> copy\n")), "{err}");
}
