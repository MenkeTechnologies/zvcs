//! `get_main_worktree()` marks the main worktree bare when
//! `repo->bare_cfg == 1 || is_bare_repository(repo)` (worktree.c:126): `core.bare = true`
//! is enough, even in a repository that has a work tree checked out. `worktree list` then
//! prints `<path> (bare)` (`bare` in the porcelain form) with no commit and no branch.
//!
//! zvcs asked only whether the repository has no work tree, and listed the commit and the
//! branch of a main worktree git calls bare.

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin.rs"]
mod twin;
use twin::Twin;

#[test]
fn core_bare_marks_the_main_worktree_bare() {
    let Some(t) = Twin::new("worktree-list-core-bare") else { return };
    for value in ["true", "on", "yes", "1"] {
        let key = format!("core.bare={value}");
        t.same(&["-c", &key, "worktree", "list"]);
        t.same(&["-c", &key, "worktree", "list", "--porcelain"]);
        t.same(&["-c", &key, "worktree", "list", "-z", "--porcelain"]);
        t.same(&["-c", &key, "worktree", "list", "-v"]);
    }
}

#[test]
fn a_false_core_bare_changes_nothing() {
    let Some(t) = Twin::new("worktree-list-core-bare-false") else { return };
    t.same(&["-c", "core.bare=false", "worktree", "list"]);
    t.same(&["worktree", "list", "--porcelain"]);
}

#[test]
fn a_linked_worktree_is_listed_beside_a_bare_main_one() {
    let Some(t) = Twin::new("worktree-list-core-bare-linked") else { return };
    t.prepare(&["worktree", "add", "-q", "../linked", "-b", "linked"]);
    t.same(&["-c", "core.bare=true", "worktree", "list", "--porcelain"]);
    t.same(&["worktree", "list", "--porcelain"]);
}
