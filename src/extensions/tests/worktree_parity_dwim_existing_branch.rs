//! `git worktree add <path>` with no `<commit-ish>` when `refs/heads/<basename>`
//! already exists.
//!
//! `dwim_branch()` (builtin/worktree.c:765-778) returns the basename itself when
//! `check_branch_ref()` + `refs_ref_exists()` find that branch, leaving
//! `new_branch` NULL; `add()` (worktree.c:890-892) then makes it `branch`, so the
//! existing branch is checked out ("checking out '<name>'") rather than created.
//! Every later `lookup_commit_reference_by_name(branch)` — worktree.c:919 and
//! `add_worktree()`:490 — asks about that name, which is why an ambiguous one
//! warns twice. `die_if_checked_out()` and the `--[no-]track` refusal
//! (worktree.c:951-952) apply as for an explicit `<commit-ish>`. zvcs always
//! tried to create the branch and failed with `a branch named '<name>' already
//! exists`.
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
    /// One commit on `main`, plus branches `foo` and `bar` and a tag `bar`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-worktree-dwim-existing-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("repo");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "one"]);
        f.run(&["branch", "foo"]);
        f.run(&["branch", "bar"]);
        f.run(&["tag", "bar"]);
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
}

#[test]
fn an_existing_branch_named_after_the_path_is_checked_out() {
    let f = Fixture::new("checkout");
    let branches = f.run(&["for-each-ref", "refs/heads"]).0;
    let (out, err, code) = f.run(&["worktree", "add", "../foo"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("HEAD is now at 699945f one\n", "Preparing worktree (checking out 'foo')\n", 0)
    );
    let wt = f.root.join("foo");
    let head = std::fs::read_to_string(f.work.join(".git/worktrees/foo/HEAD")).unwrap();
    assert_eq!(head, "ref: refs/heads/foo\n");
    assert_eq!(std::fs::read_to_string(wt.join("a")).unwrap(), "a\n");
    // No branch was created or moved.
    assert_eq!(f.run(&["for-each-ref", "refs/heads"]).0, branches);
}

#[test]
fn the_checked_out_branch_is_refused_unless_forced() {
    let f = Fixture::new("used");
    let (out, err, code) = f.run(&["worktree", "add", "../main"]);
    let want = format!(
        "Preparing worktree (checking out 'main')\nfatal: 'main' is already used by worktree at '{}'\n",
        std::fs::canonicalize(&f.work).unwrap().display()
    );
    assert_eq!((out.as_str(), err.as_str(), code), ("", want.as_str(), 128));
    assert!(!f.root.join("main").exists());
    assert!(!f.work.join(".git/worktrees").exists());

    let (_, err, code) = f.run(&["worktree", "add", "-f", "../main"]);
    assert_eq!(
        (err.as_str(), code),
        ("Preparing worktree (checking out 'main')\n", 0)
    );
}

#[test]
fn track_is_refused_and_an_ambiguous_name_warns_twice() {
    let f = Fixture::new("track");
    let (out, err, code) = f.run(&["worktree", "add", "--track", "../bar"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (
            "",
            "warning: refname 'bar' is ambiguous.\n\
             Preparing worktree (checking out 'bar')\n\
             fatal: --[no-]track can only be used if a new branch is created\n",
            128
        )
    );
    assert!(!f.root.join("bar").exists());

    let (_, err, code) = f.run(&["worktree", "add", "../bar"]);
    assert_eq!(
        (err.as_str(), code),
        (
            "warning: refname 'bar' is ambiguous.\n\
             Preparing worktree (checking out 'bar')\n\
             warning: refname 'bar' is ambiguous.\n\
             ",
            0
        )
    );
}
