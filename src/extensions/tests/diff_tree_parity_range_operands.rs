//! `cmd_diff_tree()` diffs whatever `setup_revisions()` left in `revs->pending`
//! (builtin/diff-tree.c:177-206), so every operand shape `handle_revision_arg()`
//! knows works: `A..B` pends `^A B`, `A...B` pends its merge bases and both ends
//! (three objects, which diff nothing), `B ^A` is swapped back because the second
//! tree is UNINTERESTING, a lone `^A` shows its header as `^ <oid>`, and the
//! parent marks `^!`, `^@` and `^-<n>` pend parents. `--merge-base` refuses any
//! pending object that carries a flag.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn diff_tree_takes_ranges_and_marked_operands() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("diff-tree-ranges", stock);
    for args in [
        &["diff-tree", "--stat", "main~1..main"][..],
        &["diff-tree", "main~2..main"],
        &["diff-tree", "-r", "-p", "main~2..main"],
        &["diff-tree", "-r", "main~1.."],
        &["diff-tree", "-r", "..main~1"],
        &["diff-tree", "-r", "side..main"],
        &["diff-tree", "-r", "main...side"],
        &["diff-tree", "-r", "--stat", "side...main"],
        &["diff-tree", "--name-only", "^side", "main"],
        &["diff-tree", "--name-only", "main", "^side"],
        &["diff-tree", "-r", "^main"],
        &["diff-tree", "-r", "-v", "^main"],
        &["diff-tree", "-r", "--pretty=oneline", "^main"],
        &["diff-tree", "-r", "main^!"],
        &["diff-tree", "-r", "main^@"],
        &["diff-tree", "-r", "main^-1"],
        &["diff-tree", "-r", "main^-5"],
        &["diff-tree", "-r", "main~2..main", "--", "a"],
        &["diff-tree", "-r", "--merge-base", "side..main"],
        &["diff-tree", "-r", "--merge-base", "^side", "main"],
        &["diff-tree", "-r", "--merge-base", "main...side"],
        &["diff-tree", "-r", "^nosuch", "main"],
        &["diff-tree", "-r", "nosuch..main"],
        &["diff-tree", "-r", "main~1..main", "main"],
    ] {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
    }
}
