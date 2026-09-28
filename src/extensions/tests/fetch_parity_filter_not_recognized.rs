//! `fetch --filter` against a server that does not advertise `filter`.
//!
//! `fetch-pack` checks the capability while composing its request and, when
//! the server lacks it, warns `filtering not recognized by server, ignoring`
//! and fetches everything (fetch-pack.c:1173-1178, 305-313). The warning
//! belongs to the request: a fetch with nothing to ask for says nothing. zvcs
//! dropped the filter silently.
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
    /// `work` clones `up`; `up` then gains a commit.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-fetch-filter-warning-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root, work };
        let up = f.root.join("up");
        f.run_in(&f.root, &["init", "-q", "-b", "main", "up"]);
        f.run_in(&up, &["commit", "-q", "--allow-empty", "-m", "a"]);
        f.run_in(&f.root, &["clone", "-q", "up", "work"]);
        f.run_in(&up, &["commit", "-q", "--allow-empty", "-m", "b"]);
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

const WARNING: &str = "warning: filtering not recognized by server, ignoring\n";

#[test]
fn a_request_to_a_server_without_filter_warns() {
    let f = Fixture::new("warns");
    let (_, err, code) = f.run_in(&f.work, &["fetch", "-q", "--filter=blob:none"]);
    assert_eq!((err.as_str(), code), (WARNING, 0));
    // Nothing left to ask for: no request, no warning.
    assert_eq!(
        f.run_in(&f.work, &["fetch", "-q", "--filter=blob:none"]),
        (String::new(), String::new(), 0)
    );
}

#[test]
fn a_server_that_allows_filters_gets_no_warning() {
    let f = Fixture::new("allowed");
    f.run_in(&f.root.join("up"), &["config", "uploadpack.allowFilter", "true"]);
    assert_eq!(
        f.run_in(&f.work, &["fetch", "-q", "--filter=blob:none"]),
        (String::new(), String::new(), 0)
    );
}
