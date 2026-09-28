//! A configured refspec naming a tag the repository already has.
//!
//! `find_non_local_tags()` (builtin/fetch.c:326-430) keeps a tag the
//! repository already has out of automatic tag following, so such a tag has
//! no summary row and no `FETCH_HEAD` line. A refspec that names tags —
//! `remote.<name>.fetch = refs/tags/*:refs/tags/*` — is not automatic: its
//! up-to-date tags are listed under `-v` and recorded in `FETCH_HEAD` like any
//! other ref. zvcs applied the tag-following filter to every tag row not
//! written on the command line.
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
    /// `up` has `main` tagged `v1`; `work` is a clone (so it has `v1`) with a
    /// second remote `x` fetching `refs/tags/*`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-fetch-configured-tag-refspec-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root, work };
        let up = f.root.join("up");
        f.run_in(&f.root, &["init", "-q", "-b", "main", "up"]);
        f.run_in(&up, &["commit", "-q", "--allow-empty", "-m", "a"]);
        f.run_in(&up, &["tag", "v1"]);
        f.run_in(&f.root, &["clone", "-q", "up", "work"]);
        f.run(&["config", "remote.x.url", "../up"]);
        f.run(&["config", "remote.x.fetch", "refs/tags/*:refs/tags/*"]);
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

    fn rev(&self, rev: &str) -> String {
        self.run_in(&self.root.join("up"), &["rev-parse", rev]).0.trim_end().to_owned()
    }
}

#[test]
fn an_up_to_date_tag_from_a_configured_refspec_is_listed_and_recorded() {
    let f = Fixture::new("configured");
    let (out, err, code) = f.run(&["fetch", "-v", "x"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "From ../up\n = [up to date]      v1         -> v1\n", 0)
    );
    assert_eq!(
        std::fs::read_to_string(f.work.join(".git/FETCH_HEAD")).unwrap(),
        format!("{}\tnot-for-merge\ttag 'v1' of ../up\n", f.rev("v1"))
    );
}

#[test]
fn an_up_to_date_tag_under_tag_following_is_not() {
    let f = Fixture::new("following");
    let (out, err, code) = f.run(&["fetch", "-v", "origin"]);
    assert_eq!((out.as_str(), code), ("", 0));
    assert!(!err.contains("v1"), "{err}");
    let fetch_head = std::fs::read_to_string(f.work.join(".git/FETCH_HEAD")).unwrap();
    assert!(!fetch_head.contains("tag 'v1'"), "{fetch_head}");
}
