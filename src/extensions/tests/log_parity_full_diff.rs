//! `log --full-diff`.
//!
//! `setup_revisions()` copies the walk's pathspec into `diffopt.pathspec` only
//! without `--full-diff` (revision.c:3166-3167), so the pathspec picks the
//! commits and each one's diff then shows every path it touched. Under parent
//! rewriting `simplify_commit()` saves the list `try_to_simplify_commit()` left
//! before `rewrite_parents()` replaces it (revision.c:4325-4326), keeping only
//! the first list saved (`save_parents()`), and `log_tree_diff()` diffs against
//! that (`get_saved_parents()`), not the rewritten parent. `--follow` then has
//! no diff pathspec to follow: `diff_setup_done()` dies (diff.c:5224-5225).
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
        let root = std::env::temp_dir().join(format!("zvcs-full-diff-{tag}-{}", std::process::id()));
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

/// A: x, y. B: x, y. C: y. D: x.
fn history(tag: &str) -> Fixture {
    let f = Fixture::empty(tag);
    std::fs::write(f.work.join("x"), "x1\n").unwrap();
    f.run(&["add", "x"]);
    f.commit("y", "y1\n", "A");
    std::fs::write(f.work.join("x"), "x2\n").unwrap();
    f.run(&["add", "x"]);
    f.commit("y", "y2\n", "B");
    f.commit("y", "y3\n", "C");
    f.commit("x", "x4\n", "D");
    f
}

fn ok(out: &str) -> (String, String, i32) {
    (out.to_string(), String::new(), 0)
}

#[test]
fn the_pathspec_picks_commits_not_paths() {
    let f = history("paths");
    assert_eq!(
        f.run(&["log", "--full-diff", "--name-only", "--format=%s", "main", "--", "x"]),
        ok("D\n\nx\nB\n\nx\ny\nA\n\nx\ny\n")
    );
    assert_eq!(
        f.run(&["log", "--name-only", "--format=%s", "main", "--", "x"]),
        ok("D\n\nx\nB\n\nx\nA\n\nx\n")
    );
}

#[test]
fn a_rewritten_parent_is_not_what_the_diff_is_against() {
    let f = history("saved");
    // D's display parent is rewritten to B; its diff is still against C.
    assert_eq!(
        f.run(&["log", "--full-diff", "--parents", "--name-only", "--format=%s", "main", "--", "x"]),
        ok("D\n\nx\nB\n\nx\ny\nA\n\nx\ny\n")
    );
}

#[test]
fn follow_has_no_diff_pathspec() {
    let f = history("follow");
    assert_eq!(
        f.run(&["log", "--full-diff", "--follow", "main", "--", "x"]),
        (String::new(), "fatal: --follow requires exactly one pathspec\n".to_string(), 128)
    );
}
