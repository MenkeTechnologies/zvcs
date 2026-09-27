//! `--no-walk` with a pathspec dropped the named commits that do not touch it.
//!
//! `prepare_revision_walk()` returns before `limit_list()` under `--no-walk`
//! (`if (revs->no_walk) return 0;`), and `get_revision_1()`'s
//! `REV_WALK_NO_WALK` arm runs no `try_to_simplify_commit()`
//! (revision.c:4418-4434). No commit is ever marked TREESAME, so every commit
//! named on the command line is shown whatever the pathspec says. zvcs ran its
//! path simplification anyway, in both `log` and `rev-list`, and printed only
//! the named commits that changed the path.
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
fn every_named_commit_is_shown() {
    let f = Fixture::new("named");
    let (out, err, code) = f.run(&["log", "--format=%s", "--no-walk", "main", "main~1", "--", "a"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("C\nB\n", "", 0));
    let (out, _, code) = f.run(&["rev-list", "--no-walk", "--count", "main", "main~1", "--", "a"]);
    assert_eq!((out.as_str(), code), ("2\n", 0));
    // Walking, the same pathspec does simplify B away.
    let (out, _, _) = f.run(&["log", "--format=%s", "main", "main~1", "--", "a"]);
    assert_eq!(out, "C\nA\n");
}
