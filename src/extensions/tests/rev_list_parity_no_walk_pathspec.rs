//! `--no-walk` with a pathspec drops the named commits that do not touch it.
//!
//! `prepare_revision_walk()` returns before `limit_list()` under `--no-walk`
//! (`if (revs->no_walk) return 0;`). Up to 2.55 `get_revision_1()`'s
//! `REV_WALK_NO_WALK` arm ran no `try_to_simplify_commit()`, so every named
//! commit was shown whatever the pathspec said. 2.56 restored the pathspec
//! filtering the streaming-walk refactor had lost: the arm now simplifies each
//! named commit as the reflog arm does (revision.c:4478-4482), and
//! `get_commit_action()` drops the TREESAME ones. `git show` runs the same
//! one-entry walk per commit, so it filters too.
//!
//! Expectations measured from stock git 2.56.0 under the same environment.

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
    /// A adds `a`, B adds `b`, C changes `a`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-rev-list-no-walk-pathspec-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        for (file, body, msg) in [("a", "a\n", "A"), ("b", "b\n", "B"), ("a", "a\na2\n", "C")] {
            std::fs::write(f.work.join(file), body).unwrap();
            f.run(&["add", file]);
            f.run(&["commit", "-q", "-m", msg]);
        }
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
fn named_commits_that_miss_the_path_are_dropped() {
    let f = Fixture::new("named");
    let (out, err, code) = f.run(&["log", "--format=%s", "--no-walk", "main", "main~1", "--", "a"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("C\n", "", 0));
    let (out, _, code) = f.run(&["rev-list", "--no-walk", "--count", "main", "main~1", "--", "a"]);
    assert_eq!((out.as_str(), code), ("1\n", 0));
    let (out, err, code) = f.run(&["show", "-s", "--format=%s", "main", "main~1", "--", "a"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("C\n", "", 0));
    // A topological order makes the walk limited, which skips the simplifying
    // arm (`get_walk_mode()`, revision.c:4423-4434).
    let (out, _, code) =
        f.run(&["log", "--format=%s", "--no-walk", "--topo-order", "main", "main~1", "--", "a"]);
    assert_eq!((out.as_str(), code), ("C\nB\n", 0));
    // Walking, the same pathspec does simplify B away.
    let (out, _, _) = f.run(&["log", "--format=%s", "main", "main~1", "--", "a"]);
    assert_eq!(out, "C\nA\n");
}
