//! `log -g -- <path>` and `rev-list -g -- <path>` lost reflog entries behind a
//! commit the reflog never recorded.
//!
//! Under `-g`, `get_revision_1()` takes every commit from `next_reflog_entry()`
//! and only runs `try_to_simplify_commit()` on it (revision.c:4385-4420); no
//! parent is ever followed. zvcs re-derived "what the simplified parents still
//! reach" from the tips, as it does for an ordinary walk, so an entry whose
//! parent is not itself a reflog entry — the far side of a fast-forward — cut
//! off every older entry. A root that adds the path vanished with it.
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
    /// `A` adds `f`; `topic` adds `t` (T1) then changes `f` (T2); `main`
    /// fast-forwards to T2, so its reflog is T2, A — T1 is never an entry.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-log-walk-reflogs-pathspec-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("f"), "a\n").unwrap();
        f.run(&["add", "f"]);
        f.run(&["commit", "-q", "-m", "A"]);
        f.run(&["checkout", "-q", "-b", "topic"]);
        std::fs::write(f.work.join("t"), "t\n").unwrap();
        f.run(&["add", "t"]);
        f.run(&["commit", "-q", "-m", "T1"]);
        std::fs::write(f.work.join("f"), "a\nb\n").unwrap();
        f.run(&["commit", "-q", "-am", "T2"]);
        f.run(&["checkout", "-q", "main"]);
        f.run(&["merge", "-q", "topic"]);
        f
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

#[test]
fn log_keeps_entries_behind_an_unrecorded_parent() {
    let f = Fixture::new("log");
    let out = f.run(&["log", "-g", "--format=%gd %s", "main", "--", "f"]);
    assert_eq!(out, ("main@{0} T2\nmain@{1} A\n".to_string(), String::new(), 0));
    // T2 leaves `t` alone and A has none; T1 added it but is no reflog entry.
    let out = f.run(&["log", "-g", "--format=%gd %s", "main", "--", "t"]);
    assert_eq!(out, (String::new(), String::new(), 0));
}

#[test]
fn rev_list_keeps_entries_behind_an_unrecorded_parent() {
    let f = Fixture::new("rev-list");
    let want = format!(
        "{}{}",
        f.run(&["rev-parse", "main"]).0,
        f.run(&["rev-parse", "main~2"]).0
    );
    assert_eq!(f.run(&["rev-list", "-g", "main", "--", "f"]), (want, String::new(), 0));
}
