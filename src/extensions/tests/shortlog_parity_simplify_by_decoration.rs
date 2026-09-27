//! `git shortlog --simplify-by-decoration`.
//!
//! shortlog walks with `setup_revisions()`, so it takes every revision option:
//! `--simplify-by-decoration` sets `simplify_merges`, `rewrite_parents` and
//! `prune` and clears `simplify_history` (revision.c:2445-2452), and
//! `rev_compare_tree()` answers DIFFERENT for a decorated commit and, without a
//! pathspec, SAME for any other (revision.c:789-805). zvcs rejected the option
//! as unknown, exit 129.
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
    /// `c1`..`c6` on `main` (tags `v1` on `c2`, `v2` on `c4`), a `side` branch
    /// off `c3` with `s1`, merged back with `--no-ff` as `merge`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-shortlog-simplify-deco-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        for i in 1..=6 {
            std::fs::write(f.work.join("f"), format!("{i}\n")).unwrap();
            f.run_at(1_700_000_000 + i * 10, &["add", "f"]);
            f.run_at(1_700_000_000 + i * 10, &["commit", "-q", "-m", &format!("c{i}")]);
        }
        f.run(&["tag", "v1", "HEAD~4"]);
        f.run(&["tag", "v2", "HEAD~2"]);
        f.run(&["checkout", "-q", "-b", "side", "HEAD~3"]);
        std::fs::write(f.work.join("s"), "s\n").unwrap();
        f.run(&["add", "s"]);
        f.run_at(1_700_000_100, &["commit", "-q", "-m", "s1"]);
        f.run(&["checkout", "-q", "main"]);
        f.run_at(1_700_000_200, &["merge", "-q", "--no-ff", "side", "-m", "merge"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        self.run_at(1_700_000_000, args)
    }

    fn run_at(&self, date: i64, args: &[&str]) -> (String, String, i32) {
        let stamp = format!("{date} +0000");
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@x")
            .env("GIT_COMMITTER_NAME", "C")
            .env("GIT_COMMITTER_EMAIL", "c@x")
            .env("GIT_AUTHOR_DATE", &stamp)
            .env("GIT_COMMITTER_DATE", &stamp)
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
fn only_decorated_roots_and_merges_are_counted() {
    let f = Fixture::new("count");
    let (out, err, code) = f.run(&["shortlog", "--simplify-by-decoration", "HEAD"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("A (5):\n      c1\n      c2\n      c4\n      s1\n      merge\n\n", "", 0)
    );
    // `--sparse` turns the display filter off again.
    let (out, _, code) = f.run(&["shortlog", "-s", "--simplify-by-decoration", "--sparse", "HEAD"]);
    assert_eq!((out.as_str(), code), ("     8\tA\n", 0));
}

#[test]
fn parents_are_rewritten_to_the_kept_ones() {
    let f = Fixture::new("parents");
    let (out, err, code) = f.run(&["shortlog", "--format=%h:%p", "--simplify-by-decoration", "HEAD"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (
            "A (5):\n      89af976:\n      e7b8d56:89af976\n      cef529d:e7b8d56\n      \
             11db4ec:e7b8d56\n      4e423ac:cef529d 11db4ec\n\n",
            "",
            0
        )
    );
}
