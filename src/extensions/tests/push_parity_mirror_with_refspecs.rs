//! `--mirror` / `--all` beside a refspec.
//!
//! ```c
//! if (r->mirror)
//!         inner_flags |= (TRANSPORT_PUSH_MIRROR|TRANSPORT_PUSH_FORCE);
//! if (inner_flags & TRANSPORT_PUSH_ALL) {
//!         if (argc >= 2)
//!                 die(_("--all can't be combined with refspecs"));
//! }
//! if (inner_flags & TRANSPORT_PUSH_MIRROR) {
//!         if (argc >= 2)
//!                 die(_("--mirror can't be combined with refspecs"));
//! }
//! ```
//!
//! (`cmd_push()`, builtin/push.c:805-820.) The refusal counts positionals, so
//! it comes before `set_refspecs()` parses any of them — ahead of `invalid
//! refspec` and `tag shorthand without <tag>` — and it covers the matching
//! refspec `:`, a source that matches nothing, and a mirror armed by
//! `remote.<name>.mirror`. zvcs pushed `--mirror r :` and
//! `remote.r.mirror` + `main`, reported `nosuch` as an unmatched source, and
//! let the refspec parser speak first.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::os::unix::fs::PermissionsExt;
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
    /// `r.git` holding `main` at the first commit, `w` one commit ahead with a
    /// `pre-push` hook that leaves a marker.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-push-mirror-refspecs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("w");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "--bare", "-b", "main", "../r.git"]);
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "a"]);
        f.run(&["remote", "add", "r", "../r.git"]);
        f.run(&["push", "-q", "r", "main"]);
        std::fs::write(f.work.join("a"), "b\n").unwrap();
        f.run(&["commit", "-q", "-am", "b"]);
        let hook = f.work.join(".git/hooks/pre-push");
        std::fs::write(&hook, "#!/bin/sh\ntouch \"$GIT_DIR/../hook-ran\"\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
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

    fn remote_refs(&self) -> String {
        self.run(&["--git-dir=../r.git", "for-each-ref", "--format=%(refname) %(objectname)"]).0
    }

    fn refused(&self, args: &[&str], message: &str) {
        let before = self.remote_refs();
        let (out, err, code) = self.run(args);
        assert_eq!(
            (out.as_str(), err.as_str(), code),
            ("", format!("fatal: {message}\n").as_str(), 128),
            "{args:?}"
        );
        assert_eq!(self.remote_refs(), before, "{args:?}");
        assert!(!self.work.join("hook-ran").exists(), "{args:?}");
    }
}

const MIRROR: &str = "--mirror can't be combined with refspecs";
const ALL: &str = "--all can't be combined with refspecs";

#[test]
fn mirror_refuses_every_refspec_before_parsing_it() {
    let f = Fixture::new("mirror");
    for spec in ["main", "nosuch", ":", "a:", "tag"] {
        f.refused(&["push", "--mirror", "r", spec], MIRROR);
    }
    f.refused(&["push", "--mirror", "--dry-run", "../r.git", "main"], MIRROR);
    f.refused(&["push", "--all", "r", "a:"], ALL);
}

#[test]
fn a_configured_mirror_refuses_a_refspec_too() {
    let f = Fixture::new("configured");
    f.run(&["config", "remote.r.mirror", "true"]);
    f.refused(&["push", "r", "main"], MIRROR);
    f.refused(&["push", "r", "a:"], MIRROR);
}
