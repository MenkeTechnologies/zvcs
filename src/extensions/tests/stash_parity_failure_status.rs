//! The exit status of a failing `git stash <subcommand>`, as git 2.56 reports it.
//!
//! 2.56 stopped flattening a subcommand's return value with `!!`:
//!
//! ```c
//! if (fn) {
//!         ret = fn(argc, argv, prefix, repo);
//!         if (ret < 0)
//!                 return 128;
//!         return ret;
//! }
//! ```
//!
//! (builtin/stash.c:2498-2510.) A failure (`-1`) is now 128, the status `die()`
//! uses, so that 1 is `STASH_APPLY_CONFLICT` alone (stash.h), and a positive
//! return — a child command's status — passes through. The bare `git stash` and
//! the assumed `git stash <options>` keep `!!ret`, so the same failure is still 1
//! there. Every expectation below was measured against stock git 2.56.0.
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
    /// An unborn repository on `main`.
    fn unborn(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-stash-failstatus-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f
    }

    /// One commit holding `n.txt` = `one`.
    fn new(tag: &str) -> Self {
        let f = Fixture::unborn(tag);
        f.write("n.txt", "one\n");
        f.git(&["add", "n.txt"]);
        f.git(&["commit", "-q", "-m", "base"]);
        f
    }

    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.work.join(name), body).unwrap();
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@e.co")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@e.co");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    /// Exit code and stderr.
    fn run(&self, args: &[&str]) -> (i32, String) {
        let out = self.cmd(args).output().unwrap();
        (out.status.code().unwrap(), String::from_utf8_lossy(&out.stderr).into_owned())
    }

    fn stash_count(&self) -> usize {
        let out = self.cmd(&["stash", "list"]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).lines().count()
    }

    /// A stash whose `n.txt` conflicts with a commit made on top afterwards.
    fn stash_then_diverge(&self) {
        self.write("n.txt", "stashed\n");
        self.git(&["stash", "-q"]);
        self.write("n.txt", "local\n");
        self.git(&["commit", "-q", "-a", "-m", "local"]);
    }
}

/// `-S` whose reverse apply fails: a staged edit with an overlapping unstaged
/// one on top. `do_push_stash()` returns -1 after `Cannot remove worktree
/// changes` — 128 from `git stash push`, still 1 from the assumed `git stash`.
#[test]
fn staged_push_whose_reverse_apply_fails_is_128_only_when_named() {
    let f = Fixture::new("staged");
    f.write("n.txt", "two\n");
    f.git(&["add", "n.txt"]);
    f.write("n.txt", "three\n");

    let (code, err) = f.run(&["stash", "push", "-S"]);
    assert_eq!(code, 128, "{err}");
    assert_eq!(
        err,
        "error: patch failed: n.txt:1\nerror: n.txt: patch does not apply\nCannot remove worktree changes\n"
    );
    assert_eq!(f.stash_count(), 1, "the entry is stored before the reset fails");

    // A message of its own makes the second entry a different commit: an identical
    // one (same tree, parents and second) leaves `refs/stash` where it is, and
    // `update_ref()` then writes no reflog entry for it, in stock as here.
    let (code, err) = f.run(&["stash", "-S", "-m", "again"]);
    assert_eq!(code, 1, "the assumed push keeps `!!ret`: {err}");
    assert_eq!(f.stash_count(), 2);
}

/// The refused option combinations are `do_push_stash()`'s `ret = -1`, not
/// `die()`s, so they follow the same split.
#[test]
fn refused_push_combinations_follow_the_same_split() {
    let f = Fixture::new("combo");
    f.write("n.txt", "two\n");
    f.git(&["add", "n.txt"]);

    for (args, code) in [
        (&["stash", "push", "-S", "-u"][..], 128),
        (&["stash", "save", "-S", "-u"][..], 128),
        (&["stash", "-S", "-u"][..], 1),
    ] {
        let (got, err) = f.run(args);
        assert_eq!(got, code, "git {args:?}: {err}");
        assert_eq!(err, "Can't use --staged and --include-untracked or --all at the same time\n");
    }
    assert_eq!(f.stash_count(), 0);
}

/// `apply`/`pop` exit 1 for a conflicted merge and nothing else: a merge
/// `unpack_trees()` refused over an unstaged edit is `merge_ort_nonrecursive()`'s
/// -1, so 128, and the entry is kept either way.
#[test]
fn only_a_conflicted_apply_exits_1() {
    let f = Fixture::new("apply");
    f.stash_then_diverge();

    let (code, err) = f.run(&["stash", "apply", "-q"]);
    assert_eq!(code, 1, "{err}");
    assert!(err.is_empty(), "-q silences the merge messages: {err}");

    f.git(&["reset", "-q", "--hard"]);
    // Unstaged: a *staged* edit is ours in the merge (`c_tree` is the index) and
    // would only conflict, exit 1.
    f.write("n.txt", "dirty\n");
    let (code, err) = f.run(&["stash", "pop", "-q"]);
    assert_eq!(code, 128, "{err}");
    assert_eq!(
        err,
        "error: Your local changes to the following files would be overwritten by merge:\n\tn.txt\n\
         Please commit your changes or stash them before you merge.\nAborting\n"
    );
    assert_eq!(f.stash_count(), 1, "a refused pop keeps its entry");
}

/// `get_stash_info()`'s and `get_stash_info_assert()`'s refusals are `-1`.
#[test]
fn stash_resolution_refusals_are_128() {
    let f = Fixture::new("resolve");
    for (args, err) in [
        (&["stash", "pop"][..], "No stash entries found.\n"),
        (&["stash", "apply"][..], "No stash entries found.\n"),
        (&["stash", "drop", "nosuch"][..], "error: nosuch is not a valid reference\n"),
        (&["stash", "show", "nosuch"][..], "error: nosuch is not a valid reference\n"),
    ] {
        let (code, got) = f.run(args);
        assert_eq!(code, 128, "git {args:?}: {got}");
        assert_eq!(got, err, "git {args:?}");
    }

    f.write("n.txt", "two\n");
    f.git(&["stash", "-q"]);
    let (code, err) = f.run(&["stash", "apply", "stash@{0}", "stash@{0}"]);
    assert_eq!(code, 128, "{err}");
    assert_eq!(err, "Too many revisions specified: 'stash@{0}' 'stash@{0}'\n");
    // A stash-like commit named by its id is not a `refs/stash` entry.
    let id = {
        let out = f.cmd(&["rev-parse", "stash@{0}"]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let (code, err) = f.run(&["stash", "drop", &id]);
    assert_eq!(code, 128, "{err}");
    assert_eq!(err, format!("error: '{id}' is not a stash reference\n"));
    assert_eq!(f.stash_count(), 1);
}

/// `branch_stash()` and `list_stash()` return their child's status, which now
/// passes through: `git checkout`'s and `git log`'s `die()` is 128.
#[test]
fn branch_and_list_pass_their_childs_status_through() {
    let f = Fixture::new("child");
    f.write("n.txt", "two\n");
    f.git(&["stash", "-q"]);

    let (code, err) = f.run(&["stash", "branch", "main"]);
    assert_eq!(code, 128, "{err}");
    assert_eq!(err, "fatal: a branch named 'main' already exists\n");

    let (code, err) = f.run(&["stash", "branch"]);
    assert_eq!(code, 128, "{err}");
    assert_eq!(err, "No branch name specified\n");

    let (code, err) = f.run(&["stash", "list", "--reverse"]);
    assert_eq!(code, 128, "{err}");
    assert_eq!(err, "fatal: options '--reverse' and '--walk-reflogs' cannot be used together\n");
    assert_eq!(f.stash_count(), 1, "nothing was applied or dropped");
}

/// `store`, `clear` and `create` refuse with `-1` as well.
#[test]
fn store_clear_and_create_refusals_are_128() {
    let f = Fixture::unborn("misc");
    let (code, err) = f.run(&["stash", "create"]);
    assert_eq!(code, 128, "{err}");
    assert_eq!(err, "You do not have the initial commit yet\n");

    let (code, err) = f.run(&["stash", "store"]);
    assert_eq!(code, 128, "{err}");
    assert_eq!(err, "\"git stash store\" requires one <commit> argument\n");

    let (code, err) = f.run(&["stash", "clear", "x"]);
    assert_eq!(code, 128, "{err}");
    assert_eq!(err, "error: git stash clear with arguments is unimplemented\n");
}
