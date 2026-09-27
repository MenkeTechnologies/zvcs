//! `git push -u` with no refspec still needs an upstream.
//!
//! `get_upstream_ref()` lets a branch without `branch.<name>.merge` through only
//! under `TRANSPORT_PUSH_AUTO_UPSTREAM`, which `push.autoSetupRemote` sets
//! (builtin/push.c:198, :501-504); `-u` is `TRANSPORT_PUSH_SET_UPSTREAM` and plays
//! no part in it. The same function refuses a merge value with no
//! `branch.<name>.remote`, and more than one merge value (builtin/push.c:204-222).
//! zvcs read `-u` as the auto flag, pushed, and wrote the tracking config.
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
    /// `work` on `main` with one commit, `origin` an empty bare repository.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-push-u-upstream-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "--bare", "../r.git"]);
        f.run(&["init", "-q", "-b", "main", "."]);
        f.run(&["commit", "-q", "--allow-empty", "-m", "a"]);
        f.run(&["remote", "add", "origin", "../r.git"]);
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
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
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

const NO_UPSTREAM: &str = "fatal: The current branch main has no upstream branch.
To push the current branch and set the remote as upstream, use

    git push --set-upstream origin main

To have this happen automatically for branches without a tracking
upstream, see 'push.autoSetupRemote' in 'git help config'.

";

#[test]
fn bare_push_u_dies_without_an_upstream_and_writes_nothing() {
    let f = Fixture::new("bare");
    for args in [&["push", "-u"][..], &["push", "--set-upstream", "origin"][..]] {
        let (out, err, code) = f.run(args);
        assert_eq!((out.as_str(), err.as_str(), code), ("", NO_UPSTREAM, 128), "{args:?}");
    }
    assert_eq!(f.run(&["config", "branch.main.merge"]).2, 1);
    let (refs, _, _) = f.run(&["ls-remote", "origin"]);
    assert_eq!(refs, "");
    // `push.default = current` never asks for an upstream, so `-u` pushes and records one.
    let (out, err, code) = f.run(&["-c", "push.default=current", "push", "-u"]);
    assert_eq!((out.as_str(), code), ("branch 'main' set up to track 'origin/main'.\n", 0), "{err}");
}

#[test]
fn a_merge_without_a_remote_or_two_merges_is_no_single_upstream() {
    let f = Fixture::new("merge");
    f.run(&["config", "branch.main.merge", "refs/heads/main"]);
    let (out, err, code) = f.run(&["push"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", NO_UPSTREAM, 128));

    f.run(&["config", "branch.main.remote", "origin"]);
    f.run(&["config", "--add", "branch.main.merge", "refs/heads/other"]);
    let (out, err, code) = f.run(&["push"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "fatal: The current branch main has multiple upstream branches, refusing to push.\n", 128)
    );
}
