//! A push to the branch's own remote name, where no such remote is configured.
//!
//! `setup_default_push_refspecs()` compares `remote->name` with
//! `remote_for_branch()` (builtin/push.c:260). An unconfigured `origin` is still
//! a remote named `origin` (`make_remote()`, remote.c:807, then a URL alias), so
//! under `simple` the upstream check runs and dies before any connection. zvcs
//! compared the anonymous remote's missing name, skipped the check, and died
//! "'origin' does not appear to be a git repository".
//!
//! That made remote also joins `remote_state->remotes`, so it counts when
//! `remote_for_branch()` looks for the sole remote (remote.c:677-678): a path
//! pushed to from a repository with no remotes is the branch's own remote too.
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
    /// `work` on `main` with one commit and no remote; `r.git` an empty bare repository.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-push-unconfigured-own-remote-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "--bare", "../r.git"]);
        f.run(&["init", "-q", "-b", "main", "."]);
        f.run(&["commit", "-q", "--allow-empty", "-m", "a"]);
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

#[test]
fn simple_asks_for_an_upstream_before_connecting() {
    let f = Fixture::new("simple");
    let (out, err, code) = f.run(&["push", "origin"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (
            "",
            "fatal: The current branch main has no upstream branch.
To push the current branch and set the remote as upstream, use

    git push --set-upstream origin main

To have this happen automatically for branches without a tracking
upstream, see 'push.autoSetupRemote' in 'git help config'.

",
            128
        )
    );
    // With no remote configured, the path is the sole remote.
    let (_, err, code) = f.run(&["-c", "push.default=upstream", "push", "../r.git"]);
    assert_eq!(code, 128);
    assert!(err.starts_with("fatal: The current branch main has no upstream branch.\n"), "{err}");
    assert!(err.contains("\n    git push --set-upstream ../r.git main\n"), "{err}");
    // Beside a configured remote it is one of two, so the branch's remote is `origin`.
    f.run(&["remote", "add", "a", "../r.git"]);
    let (out, err, code) = f.run(&["-c", "push.default=upstream", "push", "../r.git"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (
            "",
            "fatal: You are pushing to remote '../r.git', which is not the upstream of
your current branch 'main', without telling me what to push
to update which remote branch.
",
            128
        )
    );
}
