//! `--squash` against `--no-ff` / `--commit`: which values clash, and when.
//!
//! ```c
//! if (squash) {
//!         if (fast_forward == FF_NO)
//!                 die(_("options '%s' and '%s' cannot be used together"), "--squash", "--no-ff.");
//!         if (option_commit > 0)
//!                 die(_("options '%s' and '%s' cannot be used together"), "--squash", "--commit.");
//! ```
//!
//! (builtin/merge.c:1503-1507.) `fast_forward` is whatever `merge.ff`,
//! `branch.<name>.mergeoptions` and the command line left, last one winning,
//! and the check runs after `die_resolve_conflict("merge")` and the
//! MERGE_HEAD / CHERRY_PICK_HEAD refusals (builtin/merge.c:1472-1492). zvcs
//! keyed the check off the literal `--no-ff`/`--commit` flags and ran it
//! before any of those refusals, with `--commit` tested first.
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
    /// `main` and `side` both rewrite `file` from a common base.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-merge-squash-clash-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "base\n").unwrap();
        f.run(&["add", "file"]);
        f.run(&["commit", "-q", "-m", "base"]);
        f.run(&["checkout", "-q", "-b", "side"]);
        std::fs::write(f.work.join("file"), "side\n").unwrap();
        f.run(&["commit", "-q", "-am", "side"]);
        f.run(&["checkout", "-q", "main"]);
        std::fs::write(f.work.join("file"), "main\n").unwrap();
        f.run(&["commit", "-q", "-am", "main"]);
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
            .env("GIT_MERGE_AUTOEDIT", "no")
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

const NO_FF: &str = "fatal: options '--squash' and '--no-ff.' cannot be used together\n";
const COMMIT: &str = "fatal: options '--squash' and '--commit.' cannot be used together\n";

#[test]
fn the_effective_fast_forward_value_decides() {
    let f = Fixture::new("ff");
    for args in [
        &["-c", "merge.ff=false", "merge", "--squash", "side"][..],
        &["-c", "branch.main.mergeoptions=--no-ff", "merge", "--squash", "side"],
        // `--no-ff` is tested first.
        &["merge", "--squash", "--commit", "--no-ff", "side"],
    ] {
        assert_eq!(f.run(args), (String::new(), NO_FF.to_string(), 128), "{args:?}");
    }
    assert_eq!(f.run(&["merge", "--squash", "--commit", "side"]), (String::new(), COMMIT.to_string(), 128));
    assert!(!f.work.join(".git/SQUASH_MSG").exists());

    // The last of `--no-ff --ff` and of `--commit --no-commit` wins, so both merge.
    for args in [&["merge", "--squash", "--no-ff", "--ff", "side"][..], &["merge", "--squash", "--commit", "--no-commit", "side"]] {
        let (out, _, code) = f.run(args);
        assert_eq!(code, 1, "{args:?}");
        assert!(out.contains("Squash commit -- not updating HEAD\n"), "{args:?}: {out}");
        f.run(&["reset", "-q", "--hard"]);
    }
}

#[test]
fn an_unmerged_index_is_refused_first() {
    let f = Fixture::new("unmerged");
    assert_eq!(f.run(&["merge", "side"]).2, 1);
    let want = "error: Merging is not possible because you have unmerged files.\n\
                hint: Fix them up in the work tree, and then use 'git add/rm <file>'\n\
                hint: as appropriate to mark resolution and make a commit.\n\
                fatal: Exiting because of an unresolved conflict.\n";
    for args in [&["merge", "--squash", "--no-ff", "side"][..], &["merge", "--squash", "--commit", "side"]] {
        assert_eq!(f.run(args), (String::new(), want.to_string(), 128), "{args:?}");
    }
}
