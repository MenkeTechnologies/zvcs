//! `setup_new_branch_info_and_source_tree()` for the start-point of `checkout -b`,
//! `switch -c` and `--orphan`.
//!
//! `parse_branchname_arg()` resolves the operand with `repo_get_oid_mb()`
//! (builtin/checkout.c:1476) and then hands it to
//! `setup_new_branch_info_and_source_tree()` (builtin/checkout.c:1299-1320) on
//! every path, branch creation included. There `setup_branch_path()` resolves it
//! a second time when it is not a ref name (builtin/checkout.c:804-806), and an
//! existing `refs/heads/<operand>` replaces the resolved id
//! (builtin/checkout.c:1313-1315). zvcs did both only for the plain switch and
//! the detach, so for `-b`/`-c`/`--orphan`:
//!
//! * `amb^0` warned about an ambiguous `amb` once too few times;
//! * a name that is both a branch and a tag moved the worktree to the tag,
//!   where git moves it to the branch before `create_branch()` dies with
//!   `ambiguous object name` (branch.c:584-585).
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
    /// `main` has `f = a`, `other` has `f = b`; `amb` is a branch at `other`
    /// and a tag at `main`, so the two readings of `amb` differ.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-checkout-start-branch-info-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("f"), "a\n").unwrap();
        f.run(&["add", "f"]);
        f.run(&["commit", "-q", "-m", "a"]);
        f.run(&["checkout", "-q", "-b", "other"]);
        std::fs::write(f.work.join("f"), "b\n").unwrap();
        f.run(&["commit", "-q", "-am", "b"]);
        f.run(&["checkout", "-q", "main"]);
        f.run(&["branch", "amb", "other"]);
        f.run(&["tag", "amb", "main"]);
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
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn worktree_f(&self) -> String {
        std::fs::read_to_string(self.work.join("f")).unwrap()
    }
}

const WARN: &str = "warning: refname 'amb' is ambiguous.\n";

#[test]
fn a_branch_that_is_also_a_tag_moves_the_worktree_to_the_branch() {
    for argv in [["switch", "-c", "c"], ["checkout", "-b", "c"]] {
        let f = Fixture::new(argv[0]);
        let mut args = argv.to_vec();
        args.push("amb");
        let (out, err, code) = f.run(&args);
        let want = format!("{WARN}{WARN}fatal: ambiguous object name: 'amb'\n");
        assert_eq!((out.as_str(), err.as_str(), code), ("", want.as_str(), 128), "{argv:?}");
        // `merge_working_tree()` went to `other` (the branch), not `main` (the
        // tag); `create_branch()` then died, so `HEAD` and the refs are unmoved.
        assert_eq!(f.worktree_f(), "b\n", "{argv:?}");
        assert_eq!(f.run(&["status", "--short"]).0, "M  f\n", "{argv:?}");
        assert_eq!(f.run(&["symbolic-ref", "HEAD"]).0, "refs/heads/main\n", "{argv:?}");
        assert_eq!(f.run(&["rev-parse", "-q", "--verify", "refs/heads/c"]).2, 1, "{argv:?}");
    }
}

#[test]
fn a_non_ref_start_point_is_resolved_a_second_time() {
    let f = Fixture::new("second");
    let main = f.run(&["rev-parse", "main"]).0;
    for argv in [["checkout", "-b", "g"], ["switch", "-c", "h"]] {
        let mut args = argv.to_vec();
        args.push("amb^0");
        let (out, err, code) = f.run(&args);
        // `parse_branchname_arg()`, `setup_branch_path()`, `dwim_branch_start()`.
        let want = format!("{WARN}{WARN}{WARN}Switched to a new branch '{}'\n", argv[2]);
        assert_eq!((out.as_str(), err.as_str(), code), ("", want.as_str(), 0), "{argv:?}");
        assert_eq!(f.run(&["rev-parse", argv[2]]).0, main, "{argv:?}");
        f.run(&["checkout", "-q", "main"]);
    }
}

#[test]
fn an_orphan_start_point_is_resolved_a_second_time() {
    let f = Fixture::new("orphan");
    let (out, err, code) = f.run(&["switch", "--orphan", "o", "amb^0"]);
    let want = format!("{WARN}{WARN}fatal: '--orphan' cannot take <start-point>\n");
    assert_eq!((out.as_str(), err.as_str(), code), ("", want.as_str(), 128));

    let (out, err, code) = f.run(&["checkout", "--orphan", "o2", "amb^0"]);
    let want = format!("{WARN}{WARN}Switched to a new branch 'o2'\n");
    assert_eq!((out.as_str(), err.as_str(), code), ("", want.as_str(), 0));
    assert_eq!(f.run(&["symbolic-ref", "HEAD"]).0, "refs/heads/o2\n");
    assert_eq!(f.worktree_f(), "a\n");
}
