//! `log --maximal-only` under a pathspec and `--simplify-by-decoration`.
//!
//! `get_commit_action()` ignores a commit carrying CHILD_VISITED
//! (revision.c:4180), which `process_parents()` sets on the parents
//! `try_to_simplify_commit()` left (revision.c:1174, 1205). Under a pathspec a
//! merge TREESAME to its first parent keeps only that parent, so its other side
//! is never marked, and a tip on that side is maximal. The test comes before the
//! TREESAME one (revision.c:4221), so both apply; `--simplify-merges` and
//! `--simplify-by-decoration` limit the walk (revision.c:2439-2452), so every
//! mark is made before any commit is judged. zvcs refused the combinations as
//! not ported.
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
        let root = std::env::temp_dir().join(format!("zvcs-maximal-pathspec-{tag}-{}", std::process::id()));
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

/// A adds x and y; `side` changes x (S); `main` changes y (B), then merges
/// `side` with `-s ours` (M), keeping main's x.
fn history(tag: &str) -> Fixture {
    let f = Fixture::empty(tag);
    std::fs::write(f.work.join("x"), "x1\n").unwrap();
    f.run(&["add", "x"]);
    f.commit("y", "y1\n", "A");
    f.run(&["tag", "vA"]);
    f.run(&["checkout", "-q", "-b", "side"]);
    f.commit("x", "x2\n", "S");
    f.run(&["checkout", "-q", "main"]);
    f.commit("y", "y2\n", "B");
    f.run(&["merge", "-q", "-s", "ours", "--no-edit", "side", "-m", "M"]);
    f
}

fn ok(out: &str) -> (String, String, i32) {
    (out.to_string(), String::new(), 0)
}

#[test]
fn a_pruned_side_is_never_marked() {
    let f = history("pruned");
    assert_eq!(f.run(&["log", "--maximal-only", "--format=%s", "--all", "--", "x"]), ok("S\n"));
    // Without the prune the merge marks the side, and is itself shown.
    assert_eq!(
        f.run(&["log", "--maximal-only", "--format=%s", "--full-history", "--all", "--", "x"]),
        ok("M\n")
    );
    // Under y, M keeps B, which it marks; the side tip S leaves y alone.
    assert_eq!(f.run(&["log", "--maximal-only", "--format=%s", "--all", "--", "y"]), ok(""));
}

#[test]
fn simplify_by_decoration() {
    let f = history("decoration");
    assert_eq!(
        f.run(&["log", "--maximal-only", "--format=%s", "--simplify-by-decoration", "--all"]),
        ok("M\n")
    );
    assert_eq!(
        f.run(&["log", "--maximal-only", "--format=%s", "--simplify-by-decoration", "vA", "side"]),
        ok("S\n")
    );
}
