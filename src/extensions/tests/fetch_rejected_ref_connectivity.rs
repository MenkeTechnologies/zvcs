//! The post-fetch connectivity check only covers the refs the fetch will write.
//!
//! `store_updated_refs()` runs `check_connected()` over the ref map, but by then
//! `filter_refs()` has dropped the updates that will not be performed — a tag
//! the remote moved is not overwritten without force, and its new object is
//! never asked for. Demanding that object anyway fails the whole fetch with
//! `did not send all necessary objects` on a repository git fetches happily.
//!
//! Reproduced against a local remote: the remote's tag is force-moved to a
//! commit the client never fetches, and the client's fetch must still complete
//! and leave its own tag alone.
//!
//! Still divergent, and deliberately not asserted here: git's *automatic* tag
//! following never proposes a tag that already exists locally
//! (`find_non_local_tags()` consults the local refs first), so a plain fetch
//! says nothing about it and exits 0, while this port reports
//! `! [rejected] … (would clobber existing tag)` and exits 1 — the wording git
//! reserves for an explicit `--tags`.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    srv: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-fetchrej-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let srv = root.join("srv");
        let work = root.join("work");
        std::fs::create_dir_all(&srv).unwrap();
        let f = Fixture { root, srv, work };

        std::fs::write(f.srv.join("a.txt"), "one\n").unwrap();
        f.run_in(&f.srv, &["init", "-q", "-b", "main", "."]);
        f.run_in(&f.srv, &["config", "user.email", "t@e.co"]);
        f.run_in(&f.srv, &["config", "user.name", "t"]);
        f.run_in(&f.srv, &["add", "-A"]);
        f.run_in(&f.srv, &["commit", "-q", "-m", "one"]);
        f.run_in(&f.srv, &["tag", "v1"]);

        f.run_in(&f.root, &["clone", "-q", f.srv.to_str().unwrap(), "work"]);
        f.run_in(&f.work, &["config", "user.email", "t@e.co"]);
        f.run_in(&f.work, &["config", "user.name", "t"]);

        // The remote force-moves the tag onto a commit on a branch the clone
        // does not track, so the tag's new object is not something this fetch
        // downloads — and the tag itself is not something it may overwrite.
        f.run_in(&f.srv, &["checkout", "-q", "-b", "elsewhere"]);
        std::fs::write(f.srv.join("b.txt"), "b\n").unwrap();
        f.run_in(&f.srv, &["add", "b.txt"]);
        f.run_in(&f.srv, &["commit", "-q", "-m", "elsewhere"]);
        f.run_in(&f.srv, &["tag", "-f", "v1"]);
        f.run_in(&f.srv, &["checkout", "-q", "main"]);
        // Something for the fetch to actually bring back.
        std::fs::write(f.srv.join("a.txt"), "one\ntwo\n").unwrap();
        f.run_in(&f.srv, &["commit", "-q", "-am", "two"]);
        f
    }

    fn cmd_in(&self, dir: &PathBuf, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        c
    }

    fn run_in(&self, dir: &PathBuf, args: &[&str]) {
        let out = self.cmd_in(dir, args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = self.cmd_in(&self.work, args).output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

/// The fetch succeeds, brings the branch, and leaves the local tag where it was.
#[test]
fn a_tag_the_remote_moved_does_not_fail_the_fetch() {
    let f = Fixture::new("moved-tag");
    let tag_before = f.run(&["rev-parse", "refs/tags/v1"]).1;

    let (_code, out, err) = f.run(&["fetch", "origin"]);
    assert!(
        !err.contains("did not send all necessary objects"),
        "the connectivity check demanded an object no ref update needed: {out}{err}"
    );

    // The branch arrived…
    let (_, remote_tip, _) = f.run(&["rev-parse", "refs/remotes/origin/main"]);
    let (_, subject, _) = f.run(&["log", "-1", "--format=%s", remote_tip.trim()]);
    assert_eq!(subject.trim(), "two", "the fetch did not bring the branch: {subject}");
    // …and the tag was left alone, which is why its new object was never needed.
    assert_eq!(f.run(&["rev-parse", "refs/tags/v1"]).1, tag_before);
}

/// `pull` sits on the same check, and this is the shape that blocked it.
#[test]
fn pull_completes_with_a_moved_remote_tag() {
    let f = Fixture::new("moved-tag-pull");
    let (_code, out, err) = f.run(&["pull"]);
    assert!(
        !err.contains("did not send all necessary objects"),
        "the connectivity check blocked the pull: {out}{err}"
    );
    assert!(out.contains("Fast-forward"), "stdout: {out}{err}");
    assert_eq!(std::fs::read_to_string(f.work.join("a.txt")).unwrap(), "one\ntwo\n");
}
