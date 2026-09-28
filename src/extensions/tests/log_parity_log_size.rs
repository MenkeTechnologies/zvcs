//! `--log-size` on `log` and `show`.
//!
//! `revs->show_log_size` (revision.c:2668-2669) makes `show_log()` print
//! `log size <n>` just before the pretty-printed message, `<n>` being that
//! message's length in bytes (log-tree.c:900-903). What `show_log()` writes
//! itself — the `commit` line, the oneline id — comes first and is not counted.
//! zvcs refused the option as unsupported.
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
    fn empty(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-log-size-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        f
    }

    fn commit(&self, file: &str, body: &str, msg: &str) {
        std::fs::write(self.work.join(file), body).unwrap();
        self.run(&["add", file]);
        self.run(&["commit", "-q", "-m", msg]);
    }

    fn rev(&self, spec: &str) -> String {
        self.run(&["rev-parse", spec]).0.trim_end().to_string()
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
            .env("GIT_PAGER", "cat")
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

fn two(tag: &str) -> Fixture {
    let f = Fixture::empty(tag);
    f.commit("a", "one\n", "one");
    f.commit("a", "two\n", "two");
    f
}

const MEDIUM_BODY: &str = "Author: A U Thor <author@example.com>
Date:   Tue Nov 14 22:13:20 2023 +0000

    two
";

#[test]
fn medium_counts_from_the_author_line() {
    let f = two("medium");
    let head = f.rev("main");
    let want = format!("commit {head}\nlog size {}\n{MEDIUM_BODY}", MEDIUM_BODY.len());
    assert_eq!(f.run(&["log", "--log-size", "-1", "main"]), (want.clone(), String::new(), 0));
    assert_eq!(f.run(&["show", "--log-size", "-s", "main"]), (want, String::new(), 0));
}

#[test]
fn oneline_counts_the_subject() {
    let f = two("oneline");
    let short = |r: &str| f.run(&["rev-parse", "--short", r]).0.trim_end().to_string();
    assert_eq!(
        f.run(&["log", "--log-size", "--oneline", "main"]),
        (
            format!("{} log size 3\ntwo\n{} log size 3\none\n", short("main"), short("main~1")),
            String::new(),
            0
        )
    );
}
