//! `fetch --dry-run` receives the pack and runs the fast-forward test.
//!
//! A dry run stops short of the ref updates and `FETCH_HEAD`, not of the
//! transfer: the pack is received as for any fetch, and `update_local_ref()`
//! still decides fast-forward with `repo_in_merge_bases()`
//! (builtin/fetch.c:1047-1056), so a rewound remote branch is reported as a
//! `(forced update)` and a plain refspec onto it is rejected (exit 1). zvcs
//! skipped the pack under `--dry-run` and so had to guess: every update was a
//! fast-forward, and a tag whose object nobody sent was still listed.
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
    /// branch `copy`; then `up` rewinds `main` to `a` and commits `c` on it.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-fetch-dry-run-forced-{tag}-{}", std::process::id()));
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

    fn url(&self) -> String {
        std::fs::canonicalize(self.root.join("up")).unwrap().display().to_string()
    }
}

#[test]
fn a_rewound_branch_is_a_forced_update_and_the_pack_arrives() {
    let f = Fixture::new("forced");
    let old = f.rev("work", "origin/main");
    let new = f.rev("up", "main");
    let (out, err, code) = f.run(&["fetch", "--dry-run"]);
    assert_eq!(
        (out.as_str(), err, code),
        (
            "",
            format!(
                "From {}\n + {}...{} main       -> origin/main  (forced update)\n",
                f.url(),
                &old[..7],
                &new[..7]
            ),
            0
        )
    );
    // Nothing was written but the objects.
    assert_eq!(f.rev("work", "origin/main"), old);
    assert!(!f.work.join(".git/FETCH_HEAD").exists());
    assert_eq!(f.run(&["cat-file", "-t", &new]).0, "commit\n");
    let keeps = std::fs::read_dir(f.work.join(".git/objects/pack"))
        .unwrap()
        .filter(|e| e.as_ref().unwrap().path().extension().is_some_and(|x| x == "keep"))
        .count();
    assert_eq!(keeps, 0);
}

#[test]
fn a_plain_refspec_onto_a_rewound_branch_is_rejected() {
    let f = Fixture::new("rejected");
    let old = f.rev("work", "origin/main");
    let new = f.rev("up", "main");
    let (out, err, code) = f.run(&["fetch", "--dry-run", "origin", "main:copy"]);
    assert_eq!(
        (out.as_str(), err, code),
        (
            "",
            format!(
                "From {}\n ! [rejected] main       -> copy  (non-fast-forward)\n \
                 + {}...{} main       -> origin/main  (forced update)\n",
                f.url(),
                &old[..7],
                &new[..7]
            ),
            1
        )
    );
    assert_eq!(f.rev("work", "copy"), old);
}

#[test]
fn a_tag_outside_the_fetched_history_is_not_listed() {
    let f = Fixture::new("tags");
    let up = f.root.join("up");
    // `work` fetches `main` only; `outside` tags a commit on a branch it never asks for.
    f.run(&["config", "remote.origin.fetch", "+refs/heads/main:refs/remotes/origin/main"]);
    f.run_in(&up, &["checkout", "-q", "-b", "side"]);
    f.run_in(&up, &["commit", "-q", "--allow-empty", "-m", "s"]);
    f.run_in(&up, &["tag", "outside"]);
    f.run_in(&up, &["checkout", "-q", "main"]);
    f.run_in(&up, &["tag", "inside"]);
    let (_, err, code) = f.run(&["fetch", "--dry-run"]);
    assert_eq!(code, 0);
    // `backfill_tags()` re-proposes the first pass's tag under a dry run.
    assert!(
        err.ends_with(
            "main       -> origin/main  (forced update)\n \
             * [new tag]         inside     -> inside\n \
             * [new tag]         inside     -> inside\n"
        ),
        "{err}"
    );
}
