//! `git remote show -n`'s remote branches, wherever their tracking refs live.
//!
//! `append_ref_to_tracked_list()` (builtin/remote.c) runs every local ref that
//! is not a symref through `remote_find_tracking()` as a destination and lists
//! the source it maps back from, `abbrev_branch()`ed. A refspec may track into
//! any namespace — `refs/remotes/mirror/*`, or a tag into `refs/heads/` — and
//! the branch is listed all the same. zvcs looked only under
//! `refs/remotes/<name>/` and listed nothing for such a remote.
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
    /// `up` has `main`, `topic` and a tag `v1`; `work` is a clone with a second
    /// remote `x` tracking into `refs/remotes/mirror/` and `v1` into `refs/heads/`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-remote-show-tracked-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root, work };
        let up = f.root.join("up");
        f.run_in(&f.root, &["init", "-q", "-b", "main", "up"]);
        f.run_in(&up, &["commit", "-q", "--allow-empty", "-m", "a"]);
        f.run_in(&up, &["branch", "topic"]);
        f.run_in(&up, &["commit", "-q", "--allow-empty", "-m", "b"]);
        f.run_in(&up, &["tag", "v1"]);
        f.run_in(&f.root, &["clone", "-q", "up", "work"]);
        f.run(&["config", "remote.x.url", "../up"]);
        f.run(&["config", "remote.x.fetch", "+refs/heads/*:refs/remotes/mirror/*"]);
        f.run(&["config", "--add", "remote.x.fetch", "+refs/tags/v1:refs/heads/tagv1"]);
        f.run(&["fetch", "-q", "x"]);
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

}

#[test]
fn branches_tracked_outside_refs_remotes_name_are_listed() {
    let f = Fixture::new("elsewhere");
    assert_eq!(
        f.run(&["remote", "show", "-n", "x"]),
        (
            "* remote x\n  Fetch URL: ../up\n  Push  URL: ../up\n  HEAD branch: (not queried)\n  \
             Remote branches: (status not queried)\n    main\n    refs/tags/v1\n    topic\n  \
             Local ref configured for 'git push' (status not queried):\n    \
             (matching) pushes to (matching)\n"
                .into(),
            String::new(),
            0
        )
    );
}
