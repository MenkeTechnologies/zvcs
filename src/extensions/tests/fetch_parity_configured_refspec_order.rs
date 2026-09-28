//! Configured fetch refspecs are used in configuration order.
//!
//! `get_ref_map()` walks `remote->fetch` item by item (builtin/fetch.c:554-562)
//! and `get_fetch_map()` appends each refspec's matches in turn, so the ref
//! map — and with it `FETCH_HEAD` and the summary — follows the order the
//! refspecs were configured in; a repeated refspec adds nothing
//! (`ref_remove_duplicates()`). The vendored remote lookup sorted the
//! refspecs, so `remote.x.fetch` naming `topic` before `main` was fetched
//! `main` first.
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
    /// `up` has `main` and `topic`; `work` is a clone with a second remote `x`
    /// whose refspecs name `topic`, `main`, then `topic` again.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-fetch-refspec-order-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root, work };
        let up = f.root.join("up");
        f.run_in(&f.root, &["init", "-q", "-b", "main", "up"]);
        f.run_in(&up, &["commit", "-q", "--allow-empty", "-m", "a"]);
        f.run_in(&up, &["branch", "topic"]);
        f.run_in(&up, &["commit", "-q", "--allow-empty", "-m", "b"]);
        f.run_in(&f.root, &["clone", "-q", "up", "work"]);
        f.run(&["config", "remote.x.url", "../up"]);
        f.run(&["config", "remote.x.fetch", "+refs/heads/topic:refs/remotes/x/topic"]);
        f.run(&["config", "--add", "remote.x.fetch", "+refs/heads/main:refs/remotes/x/main"]);
        f.run(&["config", "--add", "remote.x.fetch", "+refs/heads/topic:refs/remotes/x/topic"]);
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
fn fetch_head_and_the_summary_follow_the_configured_order() {
    let f = Fixture::new("order");
    let (out, err, code) = f.run(&["fetch", "x"]);
    assert_eq!((out.as_str(), code), ("", 0));
    assert_eq!(
        err,
        "From ../up\n * [new branch]      topic      -> x/topic\n * [new branch]      main       -> x/main\n"
    );
    assert_eq!(
        std::fs::read_to_string(f.work.join(".git/FETCH_HEAD")).unwrap(),
        format!(
            "{}\tnot-for-merge\tbranch 'topic' of ../up\n{}\tnot-for-merge\tbranch 'main' of ../up\n",
            f.rev("topic"),
            f.rev("main")
        )
    );
}
