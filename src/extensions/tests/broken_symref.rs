//! A `refs/remotes/<remote>/HEAD` left pointing at a branch that no longer
//! exists.
//!
//! It is the state every clone of a repository whose default branch was renamed
//! ends up in: the symref still names `origin/master`, the ref was deleted with
//! the branch, and nothing repoints it until someone runs `git remote set-head`.
//! git treats such a ref as broken and *omits* it — `do_for_each_ref()` drops
//! refs that do not resolve — so fetching, listing and negotiating all carry on
//! as if it were not there. Only `git fsck` says anything about it.
//!
//! zvcs failed the whole fetch instead: the negotiation walk peels every local
//! ref, and one unresolvable ref ended the command with
//! `Could not follow a single level of a symbolic reference`. That leaves the
//! repository unable to fetch or pull at all until the symref is fixed by hand.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
    up: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// A clone whose `origin/HEAD` names a `master` that does not exist, and an
    /// upstream that has moved one commit ahead.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-brokensym-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let up = root.join("up");
        std::fs::create_dir_all(&up).unwrap();
        let f = Fixture { root: root.clone(), repo: root.join("repo"), up: up.clone() };
        f.git(&up, &["init", "-q", "-b", "main", "."]);
        f.git(&up, &["config", "user.email", "t@e.co"]);
        f.git(&up, &["config", "user.name", "t"]);
        std::fs::write(up.join("f"), "one\n").unwrap();
        f.git(&up, &["add", "-A"]);
        f.git(&up, &["commit", "-q", "-m", "c0"]);
        f.git(&root, &["clone", "-q", up.to_str().unwrap(), f.repo.to_str().unwrap()]);
        f.git(&f.repo, &["config", "user.email", "t@e.co"]);
        f.git(&f.repo, &["config", "user.name", "t"]);

        // The remote used to be `master`; the symref still says so.
        std::fs::write(
            f.repo.join(".git/refs/remotes/origin/HEAD"),
            "ref: refs/remotes/origin/master\n",
        )
        .unwrap();

        std::fs::write(up.join("f"), "one\ntwo\n").unwrap();
        f.git(&up, &["add", "f"]);
        f.git(&up, &["commit", "-q", "-m", "c1"]);
        f
    }

    fn cmd(&self, dir: &Path, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        c
    }

    fn git(&self, dir: &Path, args: &[&str]) {
        let out = self.cmd(dir, args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    /// `(exit code, stdout, stderr)` of a command run in the clone.
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let repo = self.repo.clone();
        let out = self.cmd(&repo, args).output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

/// The fetch completes and updates the tracking ref; the broken symref is
/// simply not part of the negotiation.
#[test]
fn a_fetch_ignores_the_broken_symref() {
    let f = Fixture::new("fetch");
    let (code, out, err) = f.run(&["fetch"]);
    assert_eq!(code, 0, "fetch failed: {out}{err}");
    assert!(err.contains("main       -> origin/main"), "stderr: {err}");

    let upstream = f.run(&["rev-parse", "refs/remotes/origin/main"]).1;
    let remote_tip = {
        let out = f.cmd(&f.up, &["rev-parse", "HEAD"]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    assert_eq!(upstream, remote_tip, "the tracking ref must have moved");
}

/// And so does the pull built on it — the case this came from.
#[test]
fn a_pull_ignores_the_broken_symref() {
    let f = Fixture::new("pull");
    let (code, out, err) = f.run(&["pull"]);
    assert_eq!(code, 0, "pull failed: {out}{err}");
    assert!(out.contains("Fast-forward"), "stdout: {out}");
    assert_eq!(std::fs::read_to_string(f.repo.join("f")).unwrap(), "one\ntwo\n");
}

/// `branch -a` and `for-each-ref` list every ref that resolves and drop the one
/// that does not, rather than printing a line pointing nowhere (or dying).
#[test]
fn ref_listings_omit_it() {
    let f = Fixture::new("listings");

    let (code, out, err) = f.run(&["branch", "-a"]);
    assert_eq!(code, 0, "branch -a failed: {out}{err}");
    assert!(out.contains("remotes/origin/main"), "stdout: {out}");
    assert!(!out.contains("origin/HEAD"), "the broken symref must not be listed: {out}");

    let (code, out, err) = f.run(&["for-each-ref", "--format=%(refname)"]);
    assert_eq!(code, 0, "for-each-ref failed: {out}{err}");
    assert!(out.contains("refs/remotes/origin/main"), "stdout: {out}");
    assert!(!out.contains("origin/HEAD"), "the broken symref must not be listed: {out}");
}

/// `git fsck` is the one command that does report it: `snapshot_ref()` is handed
/// the null id, names the ref, and sets `ERROR_REACHABLE` (exit 2) while still
/// checking the rest of the repository.
#[test]
fn fsck_reports_it_against_the_ref() {
    let f = Fixture::new("fsck");
    let (code, out, err) = f.run(&["fsck"]);
    assert_eq!(code, 2, "wrong exit: {out}{err}");
    assert_eq!(
        err,
        "error: refs/remotes/origin/HEAD: invalid sha1 pointer \
         0000000000000000000000000000000000000000\n",
        "stderr: {err}"
    );
}
