//! `fetch --porcelain` rows for refused updates.
//!
//! `print_porcelain()`/`display_ref_update()` print `<flag> <old> <new> <ref>`
//! with `old` taken from `ref->old_oid`, the value the local ref holds
//! (builtin/fetch.c). A refused update writes nothing, so its left column is
//! still the local value: the rewound branch a plain refspec may not
//! overwrite, the tag `--tags` would clobber. zvcs printed the remote's id
//! in both columns.
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
    /// `up` has `a-b` on `main`; `work` clones it and copies `origin/main` to
    /// branch `copy` and tags `a` as `t`; then `up` rewinds `main` to `a`, commits
    /// `c` on it and tags that `t`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-fetch-porcelain-rejected-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root, work };
        let up = f.root.join("up");
        f.run_in(&f.root, &["init", "-q", "-b", "main", "up"]);
        f.run_in(&up, &["commit", "-q", "--allow-empty", "-m", "a"]);
        f.run_in(&up, &["commit", "-q", "--allow-empty", "-m", "b"]);
        f.run_in(&f.root, &["clone", "-q", "up", "work"]);
        f.run(&["branch", "copy", "origin/main"]);
        f.run_in(&up, &["reset", "-q", "--hard", "HEAD~1"]);
        f.run_in(&up, &["commit", "-q", "--allow-empty", "-m", "c"]);
        f.run_in(&up, &["tag", "t"]);
        f.run(&["tag", "t", "HEAD~1"]);
        f
    }

    fn run_in(&self, dir: &std::path::Path, args: &[&str]) -> (String, String, i32) {
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
        self.run_in(&self.root.join(dir), &["rev-parse", rev]).0.trim_end().to_owned()
    }

}

#[test]
fn a_rejected_non_fast_forward_shows_the_local_value() {
    let f = Fixture::new("nonff");
    let old = f.rev("work", "copy");
    let new = f.rev("up", "main");
    let (out, _, code) = f.run(&["fetch", "--porcelain", "origin", "main:copy"]);
    assert_eq!(code, 1);
    assert_eq!(
        out.lines().next().unwrap(),
        format!("! {old} {new} refs/heads/copy")
    );
    // The dry run's row is the same.
    let (out, _, code) = f.run(&["fetch", "--porcelain", "--dry-run", "origin", "main:copy"]);
    assert_eq!(code, 1);
    assert_eq!(out.lines().next().unwrap(), format!("! {old} {new} refs/heads/copy"));
}

#[test]
fn a_clobbered_tag_shows_the_local_value() {
    let f = Fixture::new("tag");
    let old = f.rev("work", "t");
    let new = f.rev("up", "t");
    let (out, _, code) = f.run(&["fetch", "--porcelain", "--tags"]);
    assert_eq!(code, 1);
    assert!(out.contains(&format!("! {old} {new} refs/tags/t\n")), "{out}");
}
