//! `git remote remove` deletes the refs the remote's fetch refspecs own.
//!
//! `rm()` walks every ref with `add_branch_for_removal()` (builtin/remote.c:
//! 1066-1067, 573-608): a ref is a candidate only when `remote_find_tracking()`
//! maps it back through one of the removed remote's fetch refspecs
//! (`refspec_find_match()` with its negative-refspec check, refspec.c:346-466);
//! it is kept when any other remote's refspecs also have it as a destination;
//! and outside `refs/remotes/` it is never deleted — a `refs/heads/` one is
//! named in a note on stderr instead (builtin/remote.c:1073-1083).
//!
//! zvcs deleted everything under `refs/remotes/<name>/` whatever the refspecs
//! said: a mirror remote's `+refs/*:refs/*` did not protect them, a `^`
//! refspec did not either, a remote fetching into `refs/remotes/<other>/` left
//! its own refs behind, and the note never appeared.
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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-remote-remove-owned-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("t"), "a\n").unwrap();
        f.git(&["add", "t"]);
        f.git(&["commit", "-q", "-m", "a"]);
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

    /// A setup step that must succeed.
    fn git(&self, args: &[&str]) {
        let (_, err, code) = self.run(args);
        assert_eq!(code, 0, "{args:?}: {err}");
    }

    fn refs(&self) -> String {
        self.run(&["for-each-ref", "--format=%(refname)"]).0
    }
}

#[test]
fn refs_another_remote_also_fetches_into_are_kept() {
    let f = Fixture::new("mirror");
    f.git(&["remote", "add", "o1", "/nonexistent"]);
    f.git(&["remote", "add", "--mirror=fetch", "mir", "/nonexistent2"]);
    f.git(&["update-ref", "refs/remotes/o1/main", "HEAD"]);
    f.git(&["symbolic-ref", "refs/remotes/o1/HEAD", "refs/remotes/o1/main"]);

    // `mir`'s `+refs/*:refs/*` has every ref as a destination.
    let (out, err, code) = f.run(&["remote", "remove", "o1"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "", 0));
    assert_eq!(f.refs(), "refs/heads/main\nrefs/remotes/o1/HEAD\nrefs/remotes/o1/main\n");

    // Now `mir` owns them alone: the remote-tracking ones go, the branch is
    // only named.
    let (out, err, code) = f.run(&["remote", "remove", "mir"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (
            "",
            "Note: A branch outside the refs/remotes/ hierarchy was not removed;\n\
             to delete it, use:\n  git branch -d main\n",
            0
        )
    );
    assert_eq!(f.refs(), "refs/heads/main\n");
}

#[test]
fn several_branches_get_the_plural_note() {
    let f = Fixture::new("plural");
    f.git(&["branch", "b2"]);
    f.git(&["remote", "add", "--mirror=fetch", "m2", "/y"]);
    let (out, err, code) = f.run(&["remote", "remove", "m2"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (
            "",
            "Note: Some branches outside the refs/remotes/ hierarchy were not removed;\n\
             to delete them, use:\n  git branch -d b2\n  git branch -d main\n",
            0
        )
    );
    assert_eq!(f.refs(), "refs/heads/b2\nrefs/heads/main\n");
}

#[test]
fn the_refspecs_decide_not_the_remote_name() {
    let f = Fixture::new("refspecs");
    // A negative refspec keeps the ref it excludes.
    f.git(&["remote", "add", "o2", "/x"]);
    f.git(&["config", "--add", "remote.o2.fetch", "^refs/heads/x"]);
    f.git(&["update-ref", "refs/remotes/o2/x", "HEAD"]);
    f.git(&["update-ref", "refs/remotes/o2/y", "HEAD"]);
    // A remote fetching somewhere else owns nothing under its own name, and a
    // destination outside `refs/remotes/` is silently left alone.
    f.git(&["remote", "add", "o3", "/z"]);
    f.git(&["config", "remote.o3.fetch", "+refs/heads/*:refs/custom/*"]);
    f.git(&["update-ref", "refs/custom/a", "HEAD"]);
    f.git(&["update-ref", "refs/remotes/o3/b", "HEAD"]);
    // Fetching into another remote's namespace makes those refs this remote's.
    f.git(&["remote", "add", "o4", "/w"]);
    f.git(&["config", "remote.o4.fetch", "+refs/heads/*:refs/remotes/elsewhere/*"]);
    f.git(&["update-ref", "refs/remotes/elsewhere/c", "HEAD"]);

    for name in ["o2", "o3", "o4"] {
        let (out, err, code) = f.run(&["remote", "remove", name]);
        assert_eq!((out.as_str(), err.as_str(), code), ("", "", 0), "{name}");
    }
    assert_eq!(
        f.refs(),
        "refs/custom/a\nrefs/heads/main\nrefs/remotes/o2/x\nrefs/remotes/o3/b\n"
    );
}
