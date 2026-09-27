//! A bare `git push` with no remote it can name.
//!
//! `pushremote_get(NULL)` takes the name from `branch.<name>.pushRemote`,
//! `remote.pushDefault` or `branch.<name>.remote`, else the sole configured
//! remote, else `origin` (remote.c:666-706). Only the spelled-out names become a
//! URL alias; a fallback name without `remote.<name>.url` is no remote, and
//! `cmd_push()` dies "No configured push destination." (builtin/push.c:761-777,
//! remote.c:792-818). zvcs pushed to `origin` as a path, or to a remote that had
//! only a fetch refspec.
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
            .join(format!("zvcs-push-no-destination-{tag}-{}", std::process::id()));
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

const NO_DESTINATION: &str = "fatal: No configured push destination.
Either specify the URL from the command-line or configure a remote repository using

    git remote add <name> <url>

and then push using the remote name

    git push <name>

To push to multiple remotes at once, configure a remote group using

    git config remotes.<groupname> \"<remote1> <remote2>\"

and then push using the group name

    git push <groupname>

";

#[test]
fn a_fallback_name_without_a_url_is_no_destination() {
    let f = Fixture::new("fallback");
    // No remote at all: the fallback is `origin`.
    let (out, err, code) = f.run(&["push"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", NO_DESTINATION, 128));
    // Two remotes, neither named `origin`.
    f.run(&["remote", "add", "a", "../r.git"]);
    f.run(&["remote", "add", "b", "../r.git"]);
    let (out, err, code) = f.run(&["push", "--all"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", NO_DESTINATION, 128));
    // A sole remote that has a fetch refspec and no URL.
    f.run(&["remote", "remove", "a"]);
    f.run(&["remote", "remove", "b"]);
    f.run(&["config", "remote.x.fetch", "+refs/heads/*:refs/remotes/x/*"]);
    let (out, err, code) = f.run(&["push"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", NO_DESTINATION, 128));
}

#[test]
fn a_spelled_out_name_is_a_url_alias() {
    let f = Fixture::new("alias");
    f.run(&["remote", "add", "a", "../r.git"]);
    f.run(&["remote", "add", "b", "../r.git"]);
    // `remote.pushDefault` names a path, not a remote: it is pushed to as one.
    let (_, err, code) = f.run(&["-c", "remote.pushDefault=../r.git", "push", "--all"]);
    assert_eq!((err.as_str(), code), ("To ../r.git\n * [new branch]      main -> main\n", 0));
}
