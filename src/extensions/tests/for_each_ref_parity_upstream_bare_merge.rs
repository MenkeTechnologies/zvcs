//! `set_merge()` (remote.c:1798-1801) hands `branch.<name>.merge` to
//! `remote_find_tracking()` exactly as configured: a bare `main` is never
//! qualified to `refs/heads/main`, so the stock `refs/heads/*` fetch refspec
//! does not match it and `%(upstream)` stays empty, while a literal
//! `main:<dst>` refspec does match and a `^main` negative refspec vetoes it
//! (`refspec_find_negative_match()`, refspec.c:349-403).
//!
//! Every expectation was measured from stock git 2.56.0 in an identical
//! throwaway repository under the same pinned environment.
#![cfg(unix)]

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
            .join(format!("zvcs-fer-baremerge-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(root.join("gitconfig"), "").unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["commit", "-q", "--allow-empty", "-m", "a"]);
        f.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        f.git(&["config", "remote.origin.url", "./x"]);
        f.git(&["config", "remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"]);
        f.git(&["config", "branch.main.remote", "origin"]);
        f.git(&["config", "branch.main.merge", "main"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", self.root.join("gitconfig"))
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@example.invalid")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@example.invalid")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("GIT_PAGER", "cat");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn upstream(&self) -> String {
        let out = self
            .cmd(&["for-each-ref", "--format=[%(upstream)]", "refs/heads"])
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

#[test]
fn bare_merge_is_not_matched_by_a_pattern_refspec() {
    let f = Fixture::new("pattern");
    assert_eq!(f.upstream(), "[]\n");
    let out = f.cmd(&["rev-parse", "@{u}"]).output().unwrap();
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "fatal: upstream branch 'main' not stored as a remote-tracking branch\n"
    );
}

#[test]
fn bare_merge_matches_a_literal_refspec_unless_negated() {
    let f = Fixture::new("literal");
    f.git(&["config", "--add", "remote.origin.fetch", "main:refs/remotes/origin/lit"]);
    assert_eq!(f.upstream(), "[refs/remotes/origin/lit]\n");
    f.git(&["config", "--add", "remote.origin.fetch", "^main"]);
    assert_eq!(f.upstream(), "[]\n");
}
