//! `log --merge` and `rev-list --merge` were refused.
//!
//! `prepare_show_merge()` (revision.c:1975-2039) runs once the arguments are in:
//! it pends `HEAD` with `SYMMETRIC_LEFT` and the first of `MERGE_HEAD`,
//! `CHERRY_PICK_HEAD`, `REVERT_HEAD`, `REBASE_HEAD` that exists, excludes their
//! merge bases, and replaces the pathspec with the unmerged index paths the
//! user's pathspec selects — none selected is no path limit at all. Without
//! any of the four it dies.
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
    /// base (f, k); `side`: side1 changes f, side2 changes k; `main`: main1
    /// changes f, main2 adds o; then `git merge side` stops on f.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-log-merge-option-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."], 0);
        f.write("f", "a\n");
        f.write("k", "k\n");
        f.run(&["add", "."], 0);
        f.run(&["commit", "-q", "-m", "base"], 0);
        f.run(&["checkout", "-q", "-b", "side"], 0);
        f.write("f", "side\n");
        f.run(&["commit", "-q", "-am", "side1"], 10);
        f.write("k", "s2\n");
        f.run(&["commit", "-q", "-am", "side2"], 20);
        f.run(&["checkout", "-q", "main"], 30);
        f.write("f", "main\n");
        f.run(&["commit", "-q", "-am", "main1"], 30);
        f.write("o", "o\n");
        f.run(&["add", "o"], 40);
        f.run(&["commit", "-q", "-m", "main2"], 40);
        assert_eq!(f.run(&["merge", "-q", "side"], 50).2, 1);
        f
    }

    fn write(&self, path: &str, body: &str) {
        std::fs::write(self.work.join(path), body).unwrap();
    }

    fn run(&self, args: &[&str], at: u64) -> (String, String, i32) {
        let date = format!("@{} +0000", 1_700_000_000 + at);
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
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
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

    fn log(&self, args: &[&str]) -> String {
        let mut argv = vec!["log", "--format=%s"];
        argv.extend_from_slice(args);
        let (out, err, code) = self.run(&argv, 0);
        assert_eq!((err.as_str(), code), ("", 0), "{args:?}");
        out
    }
}

#[test]
fn the_commits_that_touched_the_conflicts_on_each_side() {
    let f = Fixture::new("walk");
    assert_eq!(f.log(&["--merge"]), "main1\nside1\n");
    assert_eq!(f.log(&["--merge", "--", "f"]), "main1\nside1\n");
    // A pathspec that selects no conflicted path leaves no path limit at all.
    assert_eq!(f.log(&["--merge", "--", "k"]), "main2\nmain1\nside2\nside1\n");
    // `HEAD` is the left side.
    let (out, _, code) = f.run(&["log", "--oneline", "--left-right", "--merge"], 0);
    assert_eq!(code, 0);
    let marks: Vec<char> = out.lines().filter_map(|l| l.chars().next()).collect();
    assert_eq!(marks, ['<', '>']);
    let (out, _, code) = f.run(&["rev-list", "--merge", "--left-right"], 0);
    assert_eq!(code, 0);
    let marks: Vec<char> = out.lines().filter_map(|l| l.chars().next()).collect();
    assert_eq!(marks, ['<', '>']);
}

#[test]
fn nothing_in_progress_is_refused() {
    let f = Fixture::new("none");
    f.run(&["merge", "--abort"], 0);
    let want = "fatal: --merge requires one of the pseudorefs MERGE_HEAD, CHERRY_PICK_HEAD, \
                REVERT_HEAD or REBASE_HEAD\n";
    for verb in ["log", "rev-list"] {
        assert_eq!(f.run(&[verb, "--merge"], 0), (String::new(), want.to_string(), 128), "{verb}");
    }
}
