//! A pathspec a ref may not be named: `git checkout Cargo.lock`.
//!
//! Ref validation rejects any name ending in `.lock` — that suffix belongs to
//! the ref's own lock file — so `refs/remotes/<remote>/Cargo.lock` is not
//! "absent", it is unparseable, and a lookup for it ERRORS rather than
//! answering "no such ref". The remote-tracking DWIM (`unique_remote_branch`,
//! shared by `checkout` and `switch`) propagated that error, so
//!
//!     git checkout Cargo.lock
//!
//! ended at `error: The ref name or path is not a valid ref name` for a path
//! sitting in the index. `unique_tracking_name()` (builtin/checkout.c) asks
//! `dwim_ref()`, which answers "no match" for a name it cannot parse, and
//! checkout then treats the argument as the pathspec it is.
//!
//! The trigger is a configured remote — with none, the loop never looks — so
//! this reproduced in every real Rust checkout and in none of the throwaway
//! repos a test would casually build.

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
    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }
}

fn fixture(tag: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!("zvcs-dotlock-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let f = Fixture { root, work };

    std::fs::write(f.work.join("Cargo.lock"), "v1\n").unwrap();
    f.git(&["init", "-q", "-b", "main", "."]);
    f.git(&["config", "user.email", "t@e.co"]);
    f.git(&["config", "user.name", "t"]);
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "one"]);
    // The trigger: without a remote to look under, the DWIM loop never runs and
    // the invalid name is never handed to ref validation.
    f.git(&["remote", "add", "origin", "https://example.invalid/x.git"]);
    f
}

#[test]
fn restoring_a_dot_lock_path_is_not_a_ref_lookup_failure() {
    let f = fixture("restore");
    std::fs::write(f.work.join("Cargo.lock"), "dirtied\n").unwrap();

    let out = f.cmd(&["checkout", "Cargo.lock"]).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);

    assert!(
        out.status.success(),
        "checkout of a tracked `.lock` path must succeed, got: {out:?}"
    );
    assert!(
        !err.contains("not a valid ref name"),
        "the pathspec must not be resolved as a ref name, got: {err}"
    );
    assert_eq!(
        std::fs::read_to_string(f.work.join("Cargo.lock")).unwrap(),
        "v1\n",
        "the file must come back from the index"
    );
}

/// The fix swallows a lookup failure, so the lookup that SUCCEEDS has to keep
/// working: a bare name matching exactly one `refs/remotes/<remote>/<name>`
/// still creates the local branch and sets its upstream.
#[test]
fn a_bare_name_still_dwims_to_its_remote_tracking_branch() {
    let f = fixture("dwim");
    f.git(&["update-ref", "refs/remotes/origin/feature", "HEAD"]);

    let out = f.cmd(&["checkout", "feature"]).output().unwrap();
    assert!(out.status.success(), "remote DWIM must still resolve: {out:?}");

    let head = f.cmd(&["branch", "--show-current"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&head.stdout).trim(), "feature");

    let upstream = f
        .cmd(&["config", "--get", "branch.feature.remote"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&upstream.stdout).trim(),
        "origin",
        "the DWIM'd branch must track the remote it came from"
    );
}
