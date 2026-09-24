//! `--continue` with the other command's sequence stopped.
//!
//! `sequencer_continue()` reads the todo list through `read_populate_todo()`
//! before it looks at a stopped pick (sequencer.c:5486-5500), and that function
//! refuses a list holding an instruction the resuming command does not own
//! (sequencer.c:3035-3047): `cannot revert during a cherry-pick.` /
//! `cannot cherry-pick during a revert.`, then the command's `fatal: … failed`.
//! zvcs went straight to committing the stopped pick and reported its unmerged
//! paths instead.
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
    /// `side` rewrites `file` then adds `x`; `main` rewrites `file` twice and
    /// adds `y` — so both a two-pick and a two-revert sequence stop on their
    /// first instruction.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-sequencer-wrong-action-{tag}-{}", std::process::id()));
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
        f.run(&["commit", "-q", "-am", "side1"]);
        std::fs::write(f.work.join("x"), "x\n").unwrap();
        f.run(&["add", "x"]);
        f.run(&["commit", "-q", "-m", "side2"]);
        f.run(&["checkout", "-q", "main"]);
        std::fs::write(f.work.join("file"), "main\n").unwrap();
        f.run(&["commit", "-q", "-am", "main1"]);
        std::fs::write(f.work.join("file"), "m2\n").unwrap();
        std::fs::write(f.work.join("y"), "y\n").unwrap();
        f.run(&["add", "y"]);
        f.run(&["commit", "-q", "-am", "main2"]);
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
fn revert_continue_refuses_a_stopped_cherry_pick_sequence() {
    let f = Fixture::new("pick");
    let (_, _, code) = f.run(&["cherry-pick", "side~1", "side"]);
    assert_eq!(code, 1);
    let (out, err, code) = f.run(&["revert", "--continue"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "error: cannot revert during a cherry-pick.\nfatal: revert failed\n", 128)
    );
    // Nothing was committed or cleared.
    assert!(f.work.join(".git/CHERRY_PICK_HEAD").exists());
    assert!(f.work.join(".git/sequencer/todo").exists());
}

#[test]
fn cherry_pick_continue_refuses_a_stopped_revert_sequence() {
    let f = Fixture::new("revert");
    let (_, _, code) = f.run(&["revert", "HEAD~1", "HEAD"]);
    assert_eq!(code, 1);
    let (out, err, code) = f.run(&["cherry-pick", "--continue"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "error: cannot cherry-pick during a revert.\nfatal: cherry-pick failed\n", 128)
    );
    assert!(f.work.join(".git/REVERT_HEAD").exists());
}
