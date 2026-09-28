//! `git show -g` / `--walk-reflogs`.
//!
//! `cmd_show()` starts with `rev.no_walk = 1` and runs `cmd_log_walk()` only once
//! something cleared it (builtin/log.c:686-701). A commit named after `-g` is
//! handed to `add_reflog_for_walk()` and never pended (revision.c:305-318), and
//! every walk under `-g` hands out reflog entries only (revision.c:4386-4388) —
//! so a bare `show -g main` prints nothing, while `show -g -2 main` walks the
//! reflog and prints each entry with its selector, as `log -g` does
//! (log-tree.c:835-846). A pathspec drops the TREESAME entries, an excluded
//! commit after `-g` is refused (reflog-walk.c:165-166), and `--reverse` cannot
//! accompany it (revision.c:3190-3192). zvcs refused `-g` as unsupported.
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
        let root = std::env::temp_dir().join(format!("zvcs-show-g-{tag}-{}", std::process::id()));
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

/// `main`'s reflog: one, two, three, reset back to two, four (which adds `b`).
fn reflog_fixture(tag: &str) -> Fixture {
    let f = Fixture::empty(tag);
    f.commit("a", "one\n", "one");
    f.commit("a", "two\n", "two");
    f.commit("a", "three\n", "three");
    f.run(&["reset", "-q", "--hard", "HEAD~1"]);
    f.commit("b", "four\n", "four");
    f
}

fn ok(out: &str) -> (String, String, i32) {
    (out.to_string(), String::new(), 0)
}

#[test]
fn without_a_count_nothing_is_walked() {
    let f = reflog_fixture("nowalk");
    assert_eq!(f.run(&["show", "-g", "main"]), ok(""));
    assert_eq!(f.run(&["show", "main", "-g"]), ok(""));
    assert_eq!(f.run(&["show", "--walk-reflogs"]), ok(""));
}

#[test]
fn a_count_walks_the_reflog() {
    let f = reflog_fixture("walk");
    assert_eq!(
        f.run(&["show", "-g", "-2", "-s", "--format=%gd:%gs", "main"]),
        ok("main@{0}:commit: four\nmain@{1}:reset: moving to HEAD~1\n")
    );
    let four = f.run(&["rev-parse", "--short", "main"]).0;
    let two = f.run(&["rev-parse", "--short", "main~1"]).0;
    assert_eq!(
        f.run(&["show", "--walk-reflogs", "-2", "--oneline", "-s", "main"]),
        ok(&format!(
            "{} main@{{0}}: commit: four\n{} main@{{1}}: reset: moving to HEAD~1\n",
            four.trim_end(),
            two.trim_end()
        ))
    );
    // The medium header carries the `Reflog:` lines.
    let (out, _, code) = f.run(&["show", "-g", "-1", "main"]);
    assert_eq!(code, 0);
    assert!(
        out.contains("\nReflog: main@{0} (C O Mitter <committer@example.com>)\nReflog message: commit: four\n"),
        "{out}"
    );
}

#[test]
fn a_pathspec_drops_the_treesame_entries() {
    let f = reflog_fixture("path");
    assert_eq!(
        f.run(&["show", "-g", "-2", "-s", "--format=%gd", "main", "--", "a"]),
        ok("main@{1}\nmain@{2}\n")
    );
}

#[test]
fn refusals() {
    let f = reflog_fixture("refuse");
    assert_eq!(
        f.run(&["show", "-g", "^main"]),
        (String::new(), "fatal: cannot walk reflogs for main\n".to_string(), 128)
    );
    assert_eq!(
        f.run(&["show", "-g", "--reverse", "-1", "main"]),
        (
            String::new(),
            "fatal: options '--reverse' and '--walk-reflogs' cannot be used together\n".to_string(),
            128
        )
    );
}
