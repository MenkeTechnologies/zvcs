//! `git commit --dry-run` exits 1 when the index differs from HEAD only by
//! intent-to-add entries.
//!
//! `dry_run_commit()` returns `!s->committable` (builtin/commit.c), and
//! `committable` is set by `wt_status_collect_changes_index()`, whose diff-index
//! runs with `ita_invisible_in_index = 1` (wt-status.c:677): an intent-to-add
//! entry is not in the index as far as that comparison is concerned.
//!
//! zvcs compared every stage-0 entry against HEAD's tree, so a gitlink added
//! with `-N` counted as a staged change and the dry run exited 0.
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
        let root = std::env::temp_dir()
            .join(format!("zvcs-commit-dry-run-ita-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.git(&["add", "a"]);
        f.git(&["commit", "-q", "-m", "base"]);
        f
    }

    /// An embedded repository at `name`, with a commit when `born`.
    fn embed(&self, name: &str, born: bool) {
        self.git(&["init", "-q", name]);
        if born {
            self.git(&["-C", name, "commit", "-q", "--allow-empty", "-m", "e"]);
        }
    }

    fn git(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
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

/// Two intent-to-add gitlinks: one over a repository with a commit, one over a
/// repository whose HEAD is unborn.
fn fixture(tag: &str) -> Fixture {
    let f = Fixture::new(tag);
    f.embed("emb", true);
    f.embed("emb2", false);
    let (_, _, code) = f.git(&["-c", "advice.addEmbeddedRepo=false", "add", "-N", "emb", "emb2"]);
    assert_eq!(code, 0);
    f
}

#[test]
fn only_intent_to_add_gitlinks_leave_nothing_to_commit() {
    let f = fixture("dry-run");
    let (out, _, code) = f.git(&["commit", "--dry-run", "--short"]);
    assert_eq!((out.as_str(), code), (" A emb\n A emb2\n", 1));
}
