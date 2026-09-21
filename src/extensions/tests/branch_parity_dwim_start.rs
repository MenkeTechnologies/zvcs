//! `dwim_branch_start()` (`branch.c:539-594`) is the one resolver behind a
//! creation's `<start-point>` *and* `--set-upstream-to`, and the distinctions it
//! draws are observable:
//!
//!   * `repo_get_oid_mb()` (`object-name.c:1308-1353`) reads `a...b` as the
//!     merge base of the two sides, either of which may be empty and mean
//!     `HEAD`. `git branch` is git's only caller.
//!   * A ref that is neither under `refs/heads/` nor the destination of some
//!     remote's fetch refspec is not a branch: with `--track` that is fatal
//!     (`upstream_not_branch`), and without it the ref is dropped so no
//!     `branch.<name>.remote` is recorded at all.
//!   * `--set-upstream-to`/`--unset-upstream` judge their argument *count*
//!     before looking anything up (`builtin/branch.c:941-947`, `:971-977`).
//!   * A branch some worktree's `HEAD` is on but that has no ref yet is "no
//!     commit on branch '<x>' yet", not "does not exist" (`:957-961`, `:909`).
//!   * `install_branch_config_multiple_remotes()` (`branch.c:105-116`) refuses
//!     to record a branch as its own upstream, with a warning and exit 0.
//!
//! Every expectation was measured from stock git 2.55.0 in an identical
//! throwaway repository under the same pinned environment.
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
    /// `main` at `base` → `two`, and a `side` branch forked off `base`, so
    /// `main...side` has exactly one merge base.
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("zvcs-br-dwimstart-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "base\n").unwrap();
        f.git(&["add", "a"]);
        f.git(&["commit", "-q", "-m", "base"]);
        f.git(&["tag", "base"]);
        f.git(&["branch", "side"]);
        std::fs::write(f.work.join("a"), "two\n").unwrap();
        f.git(&["commit", "-q", "-am", "two"]);
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

    fn unset(&self, key: &str) -> bool {
        self.run(&["config", key]).2 == 1
    }
}

/// `git branch <new> <a>...<b>` starts the branch at the merge base, and an
/// empty side means `HEAD`.
#[test]
fn a_three_dot_start_point_is_the_merge_base() {
    let f = Fixture::new("mb");
    f.git(&["branch", "mb", "main...side"]);
    assert_eq!(f.oid("refs/heads/mb"), f.oid("base"));

    f.git(&["branch", "mb2", "side..."]);
    assert_eq!(f.oid("refs/heads/mb2"), f.oid("base"));

    f.git(&["branch", "mb3", "...side"]);
    assert_eq!(f.oid("refs/heads/mb3"), f.oid("base"));
}

/// A remote-tracking ref no remote's fetch refspec maps onto is not a branch:
/// `--track` is fatal and no ref is created, while the same start-point without
/// `--track` creates the branch and records no tracking configuration.
#[test]
fn a_ref_outside_any_fetch_refspec_is_not_a_branch() {
    let f = Fixture::new("notabranch");
    f.git(&["config", "remote.local.url", "."]);
    f.git(&["config", "remote.local.fetch", "refs/heads/side:refs/remotes/local/side"]);
    f.git(&["update-ref", "refs/remotes/local/main", "main"]);

    let (out, err, code) = f.run(&["branch", "--track", "tracked", "local/main"]);
    assert_eq!((out.as_str(), code), ("", 128));
    assert_eq!(
        err,
        "fatal: cannot set up tracking information; starting point 'local/main' is not a branch\n"
    );
    assert_eq!(f.run(&["rev-parse", "--verify", "--quiet", "refs/heads/tracked"]).2, 1);

    f.git(&["config", "branch.autoSetupMerge", "true"]);
    f.git(&["branch", "untracked", "local/main"]);
    assert_eq!(f.oid("refs/heads/untracked"), f.oid("main"));
    assert!(f.unset("branch.untracked.remote"));
    assert!(f.unset("branch.untracked.merge"));
}

/// `--set-upstream-to` runs the same resolver with `explicit_tracking` on: a
/// spec that resolves to no ref at all is `upstream_not_branch`, not the
/// `upstream_missing` reserved for a spec that names no object.
#[test]
fn set_upstream_to_separates_missing_from_not_a_branch() {
    let f = Fixture::new("setup-to");

    let (_, err, code) = f.run(&["branch", "--set-upstream-to", "HEAD^{}"]);
    assert_eq!(code, 128);
    assert_eq!(
        err,
        "fatal: cannot set up tracking information; starting point 'HEAD^{}' is not a branch\n"
    );

    let (_, err, code) = f.run(&["branch", "--set-upstream-to", "no-such-thing"]);
    assert_eq!(code, 128);
    assert!(
        err.starts_with("fatal: the requested upstream branch 'no-such-thing' does not exist\n"),
        "{err}"
    );
}

/// The argument count is judged before the branch is looked up, so three
/// nonexistent names produce the count complaint and nothing else.
#[test]
fn upstream_options_refuse_more_than_one_operand_first() {
    let f = Fixture::new("count");

    let (out, err, code) = f.run(&["branch", "--set-upstream-to", "main", "a", "b", "c"]);
    assert_eq!((out.as_str(), code), ("", 128));
    assert_eq!(err, "fatal: too many arguments to set new upstream\n");

    let (out, err, code) = f.run(&["branch", "--unset-upstream", "a", "b", "c"]);
    assert_eq!((out.as_str(), code), ("", 128));
    assert_eq!(err, "fatal: too many arguments to unset upstream\n");
}

/// An unborn branch — named by `HEAD` with no ref behind it — is "no commit on
/// branch '<x>' yet" for `--edit-description`, `--set-upstream-to` and `-c`,
/// whether or not the name was typed out.
#[test]
fn an_unborn_branch_is_not_a_missing_branch() {
    let root = std::env::temp_dir().join(format!("zvcs-br-unborn-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let f = Fixture { root, work };
    f.git(&["init", "-q", "-b", "main", "."]);

    for args in [
        vec!["branch", "--edit-description"],
        vec!["branch", "--edit-description", "main"],
    ] {
        let (out, err, code) = f.run(&args);
        assert_eq!((out.as_str(), code), ("", 1), "{args:?}");
        assert_eq!(err, "error: no commit on branch 'main' yet\n", "{args:?}");
    }
    for args in [
        vec!["branch", "--set-upstream-to=nope"],
        vec!["branch", "-c", "new-branch"],
    ] {
        let (out, err, code) = f.run(&args);
        assert_eq!((out.as_str(), code), ("", 128), "{args:?}");
        assert_eq!(err, "fatal: no commit on branch 'main' yet\n", "{args:?}");
    }
}

/// `git branch -m` on an orphan `HEAD` renames no ref — there is none — but
/// moves the config section and re-points `HEAD`.
#[test]
fn renaming_an_orphan_head_repoints_head_and_moves_config() {
    let f = Fixture::new("orphan");
    f.git(&["checkout", "-q", "--orphan", "orphan-foo"]);
    f.git(&["config", "branch.orphan-foo.description", "kept"]);

    f.git(&["branch", "-m", "orphan-foo", "orphan-bar"]);
    assert_eq!(f.stdout(&["symbolic-ref", "HEAD"]).trim(), "refs/heads/orphan-bar");
    assert_eq!(f.stdout(&["config", "branch.orphan-bar.description"]).trim(), "kept");
    assert!(f.unset("branch.orphan-foo.description"));
    assert_eq!(f.stdout(&["branch", "--show-current"]).trim(), "orphan-bar");
}

/// A branch asked to track itself is a warning on stderr, exit 0, and no
/// configuration written.
#[test]
fn a_branch_is_not_set_as_its_own_upstream() {
    let f = Fixture::new("self");
    f.git(&["branch", "my13", "main"]);

    let (out, err, code) = f.run(&["branch", "--set-upstream-to", "refs/heads/my13", "my13"]);
    assert_eq!((out.as_str(), code), ("", 0));
    assert_eq!(err, "warning: not setting branch 'my13' as its own upstream\n");
    assert!(f.unset("branch.my13.remote"));
    assert!(f.unset("branch.my13.merge"));
}
