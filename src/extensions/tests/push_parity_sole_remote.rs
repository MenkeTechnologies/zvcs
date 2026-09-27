//! With no `<repository>` and no `branch.<name>.remote`, `git push` goes to the
//! only configured remote, whatever it is called.
//!
//! `remotes_remote_for_branch()` (remote.c:666-680):
//!
//! ```c
//! if (branch && branch->remote_name) { … return branch->remote_name; }
//! if (remote_state->remotes_nr == 1)
//!         return remote_state->remotes[0]->name;
//! return "origin";
//! ```
//!
//! is both the tail of `remotes_pushremote_for_branch()`, which picks the
//! destination, and what `setup_default_push_refspecs()` compares it with
//! (`same_remote`, builtin/push.c:253) — and it never consults
//! `remote.pushDefault`. zvcs fell back to `origin` in both places, so in a
//! repository whose one remote is not called `origin` a bare `git push` looked
//! for a repository named `origin`.
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
    /// A bare `up.git` and a work repository `w` with one commit on `main` and a
    /// remote `o` pointing at `../up.git`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-push-sole-remote-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("w");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run_in(&f.root, &["init", "-q", "--bare", "up.git"]);
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "a"]);
        f.run(&["remote", "add", "o", "../up.git"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        self.run_in(&self.work, args)
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
            .env("TZ", "UTC")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn remote_refs(&self) -> String {
        self.run(&["--git-dir=../up.git", "for-each-ref", "--format=%(refname)"]).0
    }
}

#[test]
fn a_bare_push_names_the_sole_remote_in_the_no_upstream_advice() {
    let f = Fixture::new("simple");
    let (out, err, code) = f.run(&["push"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (
            "",
            "fatal: The current branch main has no upstream branch.\n\
             To push the current branch and set the remote as upstream, use\n\
             \n    git push --set-upstream o main\n\n\
             To have this happen automatically for branches without a tracking\n\
             upstream, see 'push.autoSetupRemote' in 'git help config'.\n\n",
            128
        )
    );
    assert_eq!(f.remote_refs(), "");
}

#[test]
fn push_default_current_pushes_to_the_sole_remote() {
    let f = Fixture::new("current");
    let (out, err, code) = f.run(&["-c", "push.default=current", "push"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "To ../up.git\n * [new branch]      main -> main\n", 0)
    );
    // `push.autoSetupRemote` reaches the same remote and records it.
    let (_, _, code) = f.run(&["-c", "push.autoSetupRemote=true", "push"]);
    assert_eq!(code, 0);
    assert_eq!(f.run(&["config", "branch.main.remote"]).0, "o\n");
    assert_eq!(f.remote_refs(), "refs/heads/main\n");
}
