//! `git log --no-commit-id` with `-m` and with `-L`.
//!
//! `log_tree_diff_flush()` skips `show_log()` under `revs->no_commit_id`
//! (log-tree.c:939), and `show_log()` is what clears `opt->loginfo`. So a
//! per-parent `-m` loop never sees a header shown (`showed_log |=
//! !opt->loginfo`, log-tree.c:1156-1172), a `-L` record returns `!opt->loginfo`
//! (log-tree.c:1108-1112), and `log_tree_commit()` prints the one header, with
//! `log.parent = NULL`, after everything the diff machinery wrote
//! (log-tree.c:1191-1194). zvcs refused both combinations.
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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-log-no-commit-id-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("f"), "a\nb\n").unwrap();
        f.run(&["add", "f"]);
        f.run(&["commit", "-q", "-m", "base"]);
        f.run(&["checkout", "-q", "-b", "side"]);
        std::fs::write(f.work.join("s"), "s\n").unwrap();
        f.run(&["add", "s"]);
        f.run(&["commit", "-q", "-m", "side"]);
        f.run(&["checkout", "-q", "main"]);
        std::fs::write(f.work.join("f"), "a\nB\n").unwrap();
        f.run(&["commit", "-q", "-am", "main"]);
        f.run(&["merge", "-q", "--no-edit", "side"]);
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
fn a_merge_prints_every_parent_diff_then_one_bare_header() {
    let f = Fixture::new("merge");
    let (out, err, code) = f.run(&["log", "--no-commit-id", "-m", "--name-only", "--format=%s"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("s\nf\nMerge branch 'side'\nf\nmain\ns\nside\nf\nbase\n", "", 0)
    );
    // The parent whose diff the pathspec empties prints nothing, and the header
    // still follows the last parent.
    let (out, err, code) = f.run(&["log", "--no-commit-id", "-m", "--name-only", "--format=%s", "--", "s"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("s\nMerge branch 'side'\ns\nside\n", "", 0));
}

#[test]
fn a_line_range_diff_precedes_its_header() {
    let f = Fixture::new("range");
    let (out, err, code) = f.run(&["log", "--no-commit-id", "-L2,2:f", "--format=%s"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (
            "diff --git a/f b/f\nindex 422c2b7..55dce13 100644\n--- a/f\n+++ b/f\n@@ -2,1 +2,1 @@\n-b\n+B\nmain\n\
             diff --git a/f b/f\nnew file mode 100644\nindex 0000000..422c2b7\n--- /dev/null\n+++ b/f\n@@ -0,0 +2,1 @@\n+b\nbase\n",
            "",
            0
        )
    );
}
