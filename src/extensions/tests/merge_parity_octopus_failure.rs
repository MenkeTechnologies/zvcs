//! What an octopus does with a head `git-merge-one-file` could not resolve.
//!
//! `git-merge-octopus.sh` does not stop at the failing head. It sets
//! `OCTOPUS_FAILURE=1`, writes an empty `MRT`, and goes round the loop; the
//! refusal is raised at the *top* of the next iteration
//! (git-merge-octopus.sh:53-62) and only when another head follows:
//!
//! ```sh
//! case "$OCTOPUS_FAILURE" in
//! 1)
//!         gettextln "Automated merge did not work."
//!         gettextln "Should not be doing an octopus."
//!         exit 2
//! esac
//! ```
//!
//! So the same unresolved conflict is two different outcomes depending on
//! nothing but its position in the operand list:
//!
//! * not last — `exit 2`, the strategy-failed status, so `cmd_merge` rewinds to
//!   the pristine tree (builtin/merge.c:1854-1862), prints `Merge with strategy
//!   octopus failed.` and leaves no `MERGE_HEAD` behind;
//! * last — the loop ends at `exit "$OCTOPUS_FAILURE"`
//!   (git-merge-octopus.sh:125), which is 1: an ordinary conflicted merge with
//!   `MERGE_HEAD` recorded and the conflict in the worktree to fix.
//!
//! Every expectation was measured from stock git 2.55.0 in an identical
//! throwaway repository under the same pinned environment, stdout, stderr and
//! exit status compared separately.
#![cfg(unix)]

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
    /// `main` holding `base`, and three branches off it that each rewrite the
    /// same single line — so any two of them conflict irreconcilably, which is
    /// what `t7607-merge-state.sh` builds.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-octo-fail-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("base"), "base\n").unwrap();
        f.git(&["add", "base"]);
        f.git(&["commit", "-q", "-m", "Initial"]);
        for b in ["branch1", "branch2", "branch3"] {
            f.git(&["checkout", "-q", "-b", b, "main"]);
            std::fs::write(f.work.join("base"), format!("{b}\n")).unwrap();
            f.git(&["commit", "-q", "-a", "-m", &format!("Change on {b}")]);
        }
        f.git(&["checkout", "-q", "branch1"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
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
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = self.cmd(args).output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn stdout(&self, args: &[&str]) -> String {
        let (out, err, code) = self.run(args);
        assert_eq!((err.as_str(), code), ("", 0), "`git {args:?}`");
        out
    }

    fn oid(&self, spec: &str) -> String {
        self.stdout(&["rev-parse", spec]).trim().to_string()
    }
}

/// A head that follows the unresolvable one: exit 2, the pristine tree back,
/// nothing recorded.
#[test]
fn an_unresolved_head_with_another_still_to_merge_fails_the_strategy() {
    let f = Fixture::new("mid");
    let before = f.oid("HEAD");

    let (out, err, code) = f.run(&["merge", "branch2", "branch3"]);
    assert_eq!(code, 2, "stdout={out:?} stderr={err:?}");
    assert_eq!(
        out,
        "Trying simple merge with branch2\n\
         Simple merge did not work, trying automatic merge.\n\
         Auto-merging base\n\
         Automated merge did not work.\n\
         Should not be doing an octopus.\n"
    );
    assert_eq!(
        err,
        "ERROR: content conflict in base\n\
         fatal: merge program failed\n\
         Merge with strategy octopus failed.\n"
    );

    // `restore_state()`: no merge state, no conflict left in the index, and the
    // worktree back to what `HEAD` says.
    assert_eq!(f.oid("HEAD"), before);
    assert!(!f.work.join(".git").join("MERGE_HEAD").exists());
    assert_eq!(f.stdout(&["diff", "--name-status"]), "");
    assert_eq!(f.stdout(&["status", "--porcelain"]), "");
    assert_eq!(std::fs::read_to_string(f.work.join("base")).unwrap(), "branch1\n");
}

/// The same conflict in the *last* head is an ordinary conflicted merge:
/// `exit "$OCTOPUS_FAILURE"` is 1, `MERGE_HEAD` is recorded and the conflict
/// stays in the worktree for the user to resolve.
///
/// The shape has to put a clean head first, so `br1` touches only `g` against a
/// `main` that moved `f` — a three-way fold rather than the fast-forward a head
/// descending from `HEAD` would take.
#[test]
fn an_unresolved_last_head_is_a_conflicted_merge_not_a_failed_strategy() {
    let f = Fixture::new("last");
    // Rebuild the history: the fixture's branches all conflict with each other.
    f.git(&["checkout", "-q", "--orphan", "fresh"]);
    f.git(&["rm", "-q", "-rf", "."]);
    std::fs::write(f.work.join("f"), "1\n").unwrap();
    std::fs::write(f.work.join("g"), "1\n").unwrap();
    f.git(&["add", "f", "g"]);
    f.git(&["commit", "-q", "-m", "root"]);
    f.git(&["tag", "R"]);
    std::fs::write(f.work.join("f"), "fresh\n").unwrap();
    f.git(&["commit", "-q", "-a", "-m", "fresh"]);
    for b in ["br1", "br2"] {
        f.git(&["checkout", "-q", "-b", b, "R"]);
        std::fs::write(f.work.join("g"), format!("{b}\n")).unwrap();
        f.git(&["commit", "-q", "-a", "-m", b]);
    }
    f.git(&["checkout", "-q", "fresh"]);
    let before = f.oid("HEAD");

    let (out, err, code) = f.run(&["merge", "br1", "br2"]);
    assert_eq!(code, 1, "stdout={out:?} stderr={err:?}");
    assert_eq!(
        out,
        "Trying simple merge with br1\n\
         Trying simple merge with br2\n\
         Simple merge did not work, trying automatic merge.\n\
         Auto-merging g\n\
         Automatic merge failed; fix conflicts and then commit the result.\n"
    );
    assert_eq!(err, "ERROR: content conflict in g\nfatal: merge program failed\n");

    assert_eq!(f.oid("HEAD"), before);
    let heads = std::fs::read_to_string(f.work.join(".git").join("MERGE_HEAD")).unwrap();
    assert_eq!(heads, format!("{}\n{}\n", f.oid("br1"), f.oid("br2")));
    assert!(f.stdout(&["status", "--porcelain"]).contains("UU g"));
}
