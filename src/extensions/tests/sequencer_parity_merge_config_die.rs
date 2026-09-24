//! cherry-pick and revert die on an unreadable `merge.renameLimit`.
//!
//! `do_recursive_merge()` (sequencer.c) calls `init_ui_merge_options()` before
//! it merges, and `merge_recursive_config()` reads `merge.verbosity`,
//! `diff.renamelimit` and `merge.renamelimit` with `git_config_get_int()`,
//! which dies on a value it cannot parse — so the pick stops at 128 with the
//! `bad numeric config value` line and nothing merged. zvcs ignored the keys and
//! merged, leaving a conflict or a commit behind.
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
    /// `main` and `theirs` both rewrite `file` from a common base.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-sequencer-merge-config-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "base\n").unwrap();
        f.run(&["add", "file"]);
        f.run(&["commit", "-q", "-m", "base"]);
        f.run(&["checkout", "-q", "-b", "theirs"]);
        std::fs::write(f.work.join("file"), "theirs\n").unwrap();
        f.run(&["commit", "-q", "-am", "theirs"]);
        f.run(&["checkout", "-q", "main"]);
        std::fs::write(f.work.join("file"), "ours\n").unwrap();
        f.run(&["commit", "-q", "-am", "ours"]);
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

#[test]
fn revert_no_commit_dies_before_merging() {
    let f = Fixture::new("revert");
    let (out, err, code) =
        f.run(&["-c", "merge.renameLimit=1 ", "revert", "-n", "--no-edit", "main~1"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "fatal: bad numeric config value '1 ' for 'merge.renamelimit': invalid unit\n", 128)
    );
    assert_eq!(std::fs::read_to_string(f.work.join("file")).unwrap(), "ours\n");
    assert!(!f.work.join(".git/REVERT_HEAD").exists());
}

#[test]
fn cherry_pick_dies_before_merging() {
    let f = Fixture::new("pick");
    let (out, err, code) = f.run(&["-c", "diff.renameLimit=bogus", "cherry-pick", "theirs"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "fatal: bad numeric config value 'bogus' for 'diff.renamelimit': invalid unit\n", 128)
    );
    assert_eq!(std::fs::read_to_string(f.work.join("file")).unwrap(), "ours\n");
    assert!(!f.work.join(".git/CHERRY_PICK_HEAD").exists());
}
