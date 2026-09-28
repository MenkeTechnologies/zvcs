//! `log -g --boundary`: the boundary rows carry the last reflog entry popped.
//!
//! `show_log()` prints the reflog selector and message for every commit while
//! `opt->reflog_info` is set (log-tree.c:835-846, :877), and reads them from
//! `walk->last_commit_reflog` — the entry `next_reflog_entry()` handed out
//! last (reflog-walk.c:378-381). The boundary commits are printed after the walk,
//! so they show whichever entry that was: the final one once the reflog runs
//! dry, or the last one shown when `--max-count` stopped the walk. zvcs printed
//! them with no reflog information at all — the subject in `--oneline`, empty
//! `%gd`/`%gs`, no `Reflog:` lines.
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
        let root = std::env::temp_dir().join(format!("zvcs-log-g-boundary-{tag}-{}", std::process::id()));
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

/// `main`'s reflog: one, two, three, reset back to two, four.
fn reflog_fixture(tag: &str) -> Fixture {
    let f = Fixture::empty(tag);
    f.commit("a", "one\n", "one");
    f.commit("a", "two\n", "two");
    f.commit("a", "three\n", "three");
    f.run(&["reset", "-q", "--hard", "HEAD~1"]);
    f.commit("b", "four\n", "four");
    f
}

const FORMAT: &str = "--format=%m%s|%gd|%gs";

#[test]
fn an_exhausted_walk_reports_its_final_entry() {
    let f = reflog_fixture("dry");
    assert_eq!(
        f.run(&["log", FORMAT, "--boundary", "^main~1", "-g", "main"]),
        (
            ">four|main@{0}|commit: four\n>three|main@{2}|commit: three\n-two|main@{4}|commit (initial): one\n"
                .to_string(),
            String::new(),
            0
        )
    );
}

#[test]
fn a_capped_walk_reports_the_last_entry_shown() {
    let f = reflog_fixture("capped");
    assert_eq!(
        f.run(&["log", "-g", FORMAT, "--boundary", "-n2", "main"]),
        (
            ">four|main@{0}|commit: four\n>two|main@{1}|reset: moving to HEAD~1\n-one|main@{1}|reset: moving to HEAD~1\n"
                .to_string(),
            String::new(),
            0
        )
    );
    let (out, _, code) = f.run(&["log", "-g", "--boundary", "-1", "main"]);
    assert_eq!(code, 0);
    let boundary = out.split("commit - ").nth(1).expect("a boundary record");
    assert!(
        boundary.contains("\nReflog: main@{0} (C O Mitter <committer@example.com>)\nReflog message: commit: four\n"),
        "{out}"
    );
}
