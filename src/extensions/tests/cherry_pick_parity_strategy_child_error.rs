//! A `merge-<strategy>` child that fails to merge ends `cherry-pick` with 128.
//!
//! 2.56 folds every `try_merge_command()` status but a conflict into an error:
//!
//! ```c
//! if (res && res != 1)
//!         res = -1;
//! ```
//!
//! (sequencer.c:2488-2494), which `do_pick_commit()` returns as
//! `PICK_RESULT_ERROR` and `cmd_cherry_pick()` turns into
//! `die(_("cherry-pick failed"))` (builtin/revert.c:315-316). `git-merge-resolve`
//! refusing an `-X` it does not know exits 2, so `cherry-pick --strategy=resolve
//! -Xtheirs` now exits 128 with the `fatal:` tail; zvcs passed the child's 2
//! through, as 2.55 did. A conflict (status 1) still stops with 1 and
//! `CHERRY_PICK_HEAD`.
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
    /// `main` and `clash` both rewrite `file`; `feature` adds `side`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-cherry-pick-strategy-child-error-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "base\n").unwrap();
        f.run(&["add", "file"]);
        f.run(&["commit", "-q", "-m", "base"]);
        f.run(&["checkout", "-q", "-b", "feature"]);
        std::fs::write(f.work.join("side"), "side\n").unwrap();
        f.run(&["add", "side"]);
        f.run(&["commit", "-q", "-m", "side"]);
        f.run(&["checkout", "-q", "-b", "clash", "main"]);
        std::fs::write(f.work.join("file"), "clash\n").unwrap();
        f.run(&["commit", "-q", "-am", "clash"]);
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
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
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
fn a_child_that_cannot_merge_dies_with_cherry_pick_failed() {
    let f = Fixture::new("refused");
    let (stdout, stderr, code) = f.run(&["cherry-pick", "--strategy=resolve", "-Xtheirs", "feature"]);
    assert_eq!(code, 128, "{stderr}");
    assert_eq!(stdout, "");
    assert!(stderr.starts_with("error: unknown option `theirs'\n"), "{stderr}");
    assert!(
        stderr.ends_with("\nerror: could not apply 4af0ed7... side\nfatal: cherry-pick failed\n"),
        "{stderr}"
    );
    assert!(!f.work.join(".git/CHERRY_PICK_HEAD").exists());
    assert_eq!(f.run(&["status", "--porcelain"]).0, "");
}

#[test]
fn a_conflicting_child_still_stops_with_one() {
    let f = Fixture::new("conflict");
    let (stdout, stderr, code) = f.run(&["cherry-pick", "--strategy=resolve", "clash"]);
    assert_eq!(code, 1, "{stderr}");
    assert_eq!(
        stdout,
        "Trying simple merge.\nSimple merge failed, trying Automatic merge.\nAuto-merging file\n"
    );
    assert!(
        stderr.starts_with(
            "ERROR: content conflict in file\nfatal: merge program failed\nerror: could not apply a2cfc4e... clash\n"
        ),
        "{stderr}"
    );
    assert!(f.work.join(".git/CHERRY_PICK_HEAD").exists());
}
