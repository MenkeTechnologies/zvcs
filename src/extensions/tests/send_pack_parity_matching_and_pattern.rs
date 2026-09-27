//! `send-pack`'s matching refspec `:` and pattern refspecs.
//!
//! `cmd_send_pack()` parses its `<ref>` arguments as push refspecs and hands
//! them to `match_push_refs()` (builtin/send-pack.c:305-312). `:` is the
//! matching refspec (refspec.c:73-76) — every local branch the remote already
//! carries — and a `*` refspec expands over the local refs; `--helper-status`
//! then walks the advertisement, printing `ok <ref> up to date` for a matched
//! ref that did not move (builtin/send-pack.c:60-100). zvcs read `:` as a
//! deletion of the empty name and sent `refs/heads/` to the remote (`funny
//! refname`), reported every other ref as `error <ref> no match`, and refused
//! a pattern as an unmatched source.
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
    /// `r.git` holding `keep`, `main`, `old` at the first commit; `w` has `main`
    /// one commit ahead.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-send-pack-matching-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("w");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "--bare", "-b", "main", "../r.git"]);
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "a"]);
        f.run(&["branch", "old"]);
        f.run(&["branch", "keep"]);
        f.run(&["push", "-q", "../r.git", "main", "old", "keep"]);
        std::fs::write(f.work.join("a"), "b\n").unwrap();
        f.run(&["commit", "-q", "-am", "b"]);
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
        self.run(&["--git-dir=../r.git", "for-each-ref", "--format=%(refname)"]).0
    }
}

#[test]
fn the_matching_refspec_pushes_what_the_remote_carries() {
    let f = Fixture::new("matching");
    assert_eq!(
        f.run(&["send-pack", "--helper-status", "../r.git", ":"]),
        (
            "ok refs/heads/keep up to date\nok refs/heads/main\nok refs/heads/old up to date\n".to_string(),
            String::new(),
            0
        )
    );
    assert_eq!(f.remote_refs(), "refs/heads/keep\nrefs/heads/main\nrefs/heads/old\n");
    assert_eq!(
        f.run(&["--git-dir=../r.git", "rev-parse", "main"]).0,
        f.run(&["rev-parse", "main"]).0
    );
    // Nothing left to move.
    assert_eq!(
        f.run(&["send-pack", "--helper-status", "../r.git", "+:"]),
        (
            "ok refs/heads/keep up to date\nok refs/heads/main up to date\nok refs/heads/old up to date\n"
                .to_string(),
            "Everything up-to-date\n".to_string(),
            0
        )
    );
}

#[test]
fn a_pattern_refspec_expands_over_the_local_refs() {
    let f = Fixture::new("pattern");
    let (out, err, code) =
        f.run(&["send-pack", "--helper-status", "../r.git", "refs/heads/*:refs/heads/*"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("ok refs/heads/keep up to date\nok refs/heads/main\nok refs/heads/old up to date\n", "", 0)
    );
    let (_, err, code) = f.run(&["send-pack", "../r.git", "refs/heads/*:refs/heads/x/*"]);
    assert_eq!(
        (err.as_str(), code),
        (
            "To ../r.git\n \
             * [new branch]      keep -> x/keep\n \
             * [new branch]      main -> x/main\n \
             * [new branch]      old -> x/old\n",
            0
        )
    );
    assert_eq!(
        f.remote_refs(),
        "refs/heads/keep\nrefs/heads/main\nrefs/heads/old\nrefs/heads/x/keep\nrefs/heads/x/main\nrefs/heads/x/old\n"
    );
}
