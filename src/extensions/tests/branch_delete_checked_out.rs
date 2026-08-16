//! Which worktree holds a branch, and the path `git branch -d` names when it refuses.
//!
//! `delete_branches()` (builtin/branch.c) refuses on `branch_checked_out(name)`, whose
//! map `prepare_checked_out_branches()` (branch.c) fills from every non-bare worktree:
//! the branch its `HEAD` names, the branch an interrupted rebase will return to
//! (`rebase-merge/head-name`, `rebase-apply/head-name`), and the branch a bisect started
//! from (`BISECT_START`, guarded by `BISECT_LOG`). The value stored is `wt->path`, which
//! `get_main_worktree()` builds as `real_path(get_git_common_dir())` with a trailing
//! `/.git` cut off — absolute however the repository was discovered.
//!
//! Every expectation below was read off stock git 2.55.0.
#![cfg(unix)]

use std::path::{Path, PathBuf};
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
    /// `main` with one commit and an idle `feat` branch beside it.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-branchheld-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        std::fs::write(f.work.join("f.txt"), b"a\n").unwrap();
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "one"]);
        f.git(&["branch", "feat"]);
        f
    }

    fn cmd_in(&self, dir: &Path, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_EDITOR", "true")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@e.co")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@e.co");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd_in(&self.work.clone(), args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    /// `(succeeded, stdout, stderr)` of a run in `dir`, which may legitimately fail.
    fn try_git_in(&self, dir: &Path, args: &[&str]) -> (bool, String, String) {
        let out = self.cmd_in(dir, args).output().unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        )
    }

    fn branch_exists(&self, name: &str) -> bool {
        self.try_git_in(&self.work, &["rev-parse", "--verify", "-q", &format!("refs/heads/{name}")])
            .0
    }

    /// The absolute, symlink-resolved path git prints for a checkout — `real_path()`.
    fn real(path: &Path) -> String {
        std::fs::canonicalize(path).unwrap().to_str().unwrap().to_string()
    }
}

/// The refusal names the worktree, not the current directory. `repo.workdir()` is
/// relative whenever the repository was discovered by walking up from the process's
/// working directory, which printed `used by worktree at '.'` from the top of the
/// checkout and `'../..'` from two levels down; git's `wt->path` is neither.
#[test]
fn refusal_names_the_absolute_worktree_path_from_a_subdirectory() {
    let f = Fixture::new("subdir-path");
    let deep = f.work.join("sub/dir");
    std::fs::create_dir_all(&deep).unwrap();

    let (ok, _, err) = f.try_git_in(&deep, &["branch", "-D", "main"]);

    assert!(!ok, "the checked-out branch cannot be deleted");
    assert_eq!(
        err,
        format!(
            "error: cannot delete branch 'main' used by worktree at '{}'",
            Fixture::real(&f.work)
        )
    );
    assert!(f.branch_exists("main"), "the refusal left the branch in place");
}

/// The map covers *every* worktree, so a branch checked out in a linked one is refused
/// from the main worktree — and the path reported is that linked checkout's, read from
/// `worktrees/<id>/gitdir`.
#[test]
fn a_branch_held_by_a_linked_worktree_cannot_be_deleted() {
    let f = Fixture::new("linked");
    let linked = f.root.join("linked");
    f.git(&["worktree", "add", "-q", linked.to_str().unwrap(), "feat"]);

    let (ok, _, err) = f.try_git_in(&f.work, &["branch", "-D", "feat"]);

    assert!(!ok, "another worktree holds `feat`");
    assert_eq!(
        err,
        format!(
            "error: cannot delete branch 'feat' used by worktree at '{}'",
            Fixture::real(&linked)
        )
    );
    assert!(f.branch_exists("feat"), "the branch survived");
}

/// An interrupted rebase leaves `HEAD` detached, so the branch it will return to is held
/// by `rebase-merge/head-name` alone — the entry `prepare_checked_out_branches()` adds
/// beyond `wt->head_ref`.
#[test]
fn a_branch_an_interrupted_rebase_will_return_to_cannot_be_deleted() {
    let f = Fixture::new("rebase");
    // Conflicting edits to the same line on either side of the fork.
    f.git(&["checkout", "-q", "feat"]);
    std::fs::write(f.work.join("f.txt"), b"feat\n").unwrap();
    f.git(&["commit", "-q", "-am", "feat"]);
    f.git(&["checkout", "-q", "main"]);
    std::fs::write(f.work.join("f.txt"), b"main\n").unwrap();
    f.git(&["commit", "-q", "-am", "main"]);
    f.git(&["checkout", "-q", "feat"]);

    let (rebased, _, _) = f.try_git_in(&f.work, &["rebase", "main"]);
    assert!(!rebased, "the rebase must stop on the conflict");
    assert!(
        f.work.join(".git/rebase-merge/head-name").exists(),
        "the interrupted rebase records the branch it will return to"
    );
    let (symbolic, _, _) = f.try_git_in(&f.work, &["symbolic-ref", "-q", "HEAD"]);
    assert!(!symbolic, "HEAD is detached, so only the rebase state holds `feat`");

    let (ok, _, err) = f.try_git_in(&f.work, &["branch", "-D", "feat"]);

    assert!(!ok, "the rebase still owns `feat`");
    assert_eq!(
        err,
        format!(
            "error: cannot delete branch 'feat' used by worktree at '{}'",
            Fixture::real(&f.work)
        )
    );
    assert!(f.branch_exists("feat"));
}

/// A bisect detaches `HEAD` too; `BISECT_START` holds the branch it began on.
#[test]
fn a_branch_a_bisect_started_from_cannot_be_deleted() {
    let f = Fixture::new("bisect");
    for n in 2..=5 {
        std::fs::write(f.work.join("f.txt"), format!("{n}\n")).unwrap();
        f.git(&["commit", "-q", "-am", &format!("c{n}")]);
    }
    f.git(&["bisect", "start", "HEAD", "HEAD~4"]);
    assert!(f.work.join(".git/BISECT_LOG").exists(), "a bisect is in progress");
    let (symbolic, _, _) = f.try_git_in(&f.work, &["symbolic-ref", "-q", "HEAD"]);
    assert!(!symbolic, "bisect checked out a midpoint, so HEAD is detached");

    let (ok, _, err) = f.try_git_in(&f.work, &["branch", "-D", "main"]);

    assert!(!ok, "the bisect still owns `main`");
    assert_eq!(
        err,
        format!(
            "error: cannot delete branch 'main' used by worktree at '{}'",
            Fixture::real(&f.work)
        )
    );
    assert!(f.branch_exists("main"));
}

/// The guard against over-refusing: a branch no worktree, rebase or bisect holds is
/// deleted as before, even with another worktree open beside it.
#[test]
fn a_branch_no_worktree_holds_is_still_deleted() {
    let f = Fixture::new("free");
    let linked = f.root.join("linked");
    f.git(&["worktree", "add", "-q", linked.to_str().unwrap(), "feat"]);
    f.git(&["branch", "side"]);

    let (ok, out, err) = f.try_git_in(&f.work, &["branch", "-D", "side"]);

    assert!(ok, "nothing holds `side`: {err}");
    assert!(out.starts_with("Deleted branch side (was "), "unexpected output: {out}");
    assert!(!f.branch_exists("side"));
}

/// A bare repository contributes no worktree at all — `prepare_checked_out_branches()`
/// skips `wt->is_bare` — so the branch its `HEAD` names stays deletable.
#[test]
fn a_bare_repositorys_head_branch_is_still_deletable() {
    let f = Fixture::new("bare");
    let bare = f.root.join("bare.git");
    f.git(&["clone", "-q", "--bare", ".", bare.to_str().unwrap()]);

    let (ok, out, err) = f.try_git_in(&bare, &["branch", "-D", "main"]);

    assert!(ok, "a bare repository's HEAD is not a checkout: {err}");
    assert!(out.starts_with("Deleted branch main (was "), "unexpected output: {out}");
    assert!(!bare.join("refs/heads/main").exists());
}
