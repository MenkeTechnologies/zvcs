//! `--test-untracked-cache` calls `setup_work_tree()` before it probes the directory
//! (`case UC_TEST`, builtin/update-index.c), so run from inside the git directory — where
//! there is no work tree — it dies instead of testing the mtime of `.git/info`.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use twin_repo::Side;

fn world(label: &str) -> Option<(Side, Side)> {
    let stock = stock_git::stock_git()?;
    Some(twin_repo::pair(label, stock))
}

#[test]
fn test_untracked_cache_inside_the_git_dir_has_no_work_tree() {
    let Some((s, z)) = world("uc-test-no-worktree") else { return };
    let in_info = |side: &Side, args: &[&str]| side.run_in(&side.repo().join(".git/info"), &[], args);
    let want = in_info(&s, &["update-index", "--test-untracked-cache"]);
    assert_eq!((want.code, want.stderr.as_str()), (128, "fatal: this operation must be run in a work tree\n"));
    assert_eq!(in_info(&z, &["update-index", "--test-untracked-cache"]), want);
}

#[test]
fn test_untracked_cache_in_the_work_tree_still_probes() {
    let Some((s, z)) = world("uc-test-worktree") else { return };
    let want = s.git(&["update-index", "--test-untracked-cache"]);
    assert_eq!(want.code, 0, "{want:?}");
    assert_eq!(z.git(&["update-index", "--test-untracked-cache"]), want);
}
