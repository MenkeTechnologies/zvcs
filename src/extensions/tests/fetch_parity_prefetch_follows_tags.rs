//! `fetch --prefetch` still follows tags.
//!
//! `filter_prefetch_refspec()` (builtin/fetch.c:437-480) moves every
//! destination under `refs/prefetch/` and drops the refspecs that would land
//! in `refs/tags/`, but the rewritten refspecs keep destinations, so
//! `get_ref_map()` still sets `*autotags` (builtin/fetch.c:556-558) and a tag
//! pointing into the fetched history is followed into `refs/tags/` — which is
//! why `git maintenance`'s prefetch task adds `--no-tags`. zvcs switched tag
//! following off under `--prefetch`.
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
    /// `work` clones `up`; `up` then gains a commit tagged `t`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-fetch-prefetch-tags-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root, work };
        let up = f.root.join("up");
        f.run_in(&f.root, &["init", "-q", "-b", "main", "up"]);
        f.run_in(&up, &["commit", "-q", "--allow-empty", "-m", "a"]);
        f.run_in(&f.root, &["clone", "-q", "up", "work"]);
        f.run_in(&up, &["commit", "-q", "--allow-empty", "-m", "b"]);
        f.run_in(&up, &["tag", "t"]);
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
}

#[test]
fn prefetch_follows_a_tag_into_refs_tags() {
    let f = Fixture::new("follows");
    let (out, err, code) = f.run_in(&f.work, &["fetch", "--prefetch"]);
    assert_eq!((out.as_str(), code), ("", 0));
    assert!(
        err.ends_with(
            " * [new branch]      main       -> refs/prefetch/remotes/origin/main\n \
             * [new tag]         t          -> t\n"
        ),
        "{err}"
    );
    assert_eq!(f.run_in(&f.work, &["tag"]).0, "t\n");
}

#[test]
fn prefetch_with_no_tags_does_not() {
    let f = Fixture::new("notags");
    let (_, err, code) = f.run_in(&f.work, &["fetch", "--prefetch", "--no-tags"]);
    assert_eq!(code, 0);
    assert!(!err.contains("[new tag]"), "{err}");
    assert_eq!(f.run_in(&f.work, &["tag"]).0, "");
}
