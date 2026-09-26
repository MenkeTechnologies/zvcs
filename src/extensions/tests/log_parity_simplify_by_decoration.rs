//! `--simplify-by-decoration` is `--simplify-merges` over a decoration test.
//!
//! The option sets `simplify_merges`, `rewrite_parents`, `prune` and clears
//! `simplify_history` (revision.c:2445-2452), so every commit goes through
//! `try_to_simplify_commit()`, and `rev_compare_tree()` answers before looking
//! at a tree: `REV_TREE_DIFFERENT` for a decorated commit, `REV_TREE_SAME` for
//! any other once no pathspec is given (revision.c:789-805). A merge whose
//! parents are all TREESAME is therefore TREESAME itself, and
//! `simplify_merges()` reduces it away like any other. A root commit is judged
//! by `rev_same_tree_as_empty()`, which never asks about decorations, so an
//! untagged root that adds nothing a pathspec selects is dropped too.
//!
//! zvcs kept every merge and every root without comparing anything, so
//! `log --simplify-by-decoration` showed undecorated merges and roots and
//! printed their real parents.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

//! walked HEAD limited to it.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

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
    /// `base` (tagged `v1`), `mainline` on `main`, `sidework` on `side` from
    /// `v1`, the `merge` of `side` into `main`, then an untagged `tip`. Every
    /// commit but `base` adds `file` content, so a pathspec can select them.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-log-simplify-deco-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        f.run(&["commit", "-q", "--allow-empty", "-m", "base"]);
        f.run(&["tag", "v1"]);
        f.commit_file("main.txt", "mainline");
        f.run(&["checkout", "-q", "-b", "side", "v1"]);
        f.commit_file("side.txt", "sidework");
        f.run(&["checkout", "-q", "main"]);
        f.run(&["merge", "-q", "--no-ff", "-m", "merge", "side"]);
        f.commit_file("main.txt", "tip");
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
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("GIT_MERGE_AUTOEDIT", "no")
            .env("GIT_PAGER", "cat")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn stdout(&self, args: &[&str]) -> String {
        let (out, err, code) = self.run(args);
        assert_eq!((err.as_str(), code), ("", 0), "{args:?}");
        out
    }

    /// Write `subject` into `path` and commit it with that subject.
    fn commit_file(&self, path: &str, subject: &str) {
        std::fs::write(self.work.join(path), format!("{subject}\n")).unwrap();
        self.stdout(&["add", path]);
        self.stdout(&["commit", "-q", "-m", subject]);
    }
}

#[test]
fn undecorated_merges_and_roots_are_simplified_away() {
    let f = Fixture::new("all");
    let side = f.stdout(&["rev-parse", "side"]);
    assert_eq!(f.stdout(&["log", "--simplify-by-decoration", "--format=%s", "--all"]), "tip\nsidework\n");
    // The rewritten ancestry: `tip` hangs straight off `side`, and `sidework`'s
    // own parent, the untagged root, is gone.
    assert_eq!(
        f.stdout(&["log", "--simplify-by-decoration", "--format=%s:%P", "main"]),
        format!("tip:{}sidework:\n", side)
    );
    assert_eq!(
        f.stdout(&["log", "--simplify-by-decoration", "--graph", "--format=%s", "--all"]),
        "* tip\n* sidework\n"
    );
}
