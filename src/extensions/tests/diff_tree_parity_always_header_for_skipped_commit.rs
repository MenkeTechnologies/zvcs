//! `log_tree_commit()` prints the commit header for a commit `log_tree_diff()` showed nothing
//! for when `--always` set `always_show_header`: a root commit without `--root`, and a merge
//! without `-m`/`-c`. zvcs returned before printing it.
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
fn always_prints_the_header_of_a_root_and_of_a_merge_that_show_no_diff() {
    let Some((s, z)) = world("diff-tree-always") else { return };
    for side in [&s, &z] {
        side.git(&["checkout", "-q", "-b", "other", "main~2"]);
        side.write("b", "b\n");
        side.git(&["add", "b"]);
        side.git(&["commit", "-q", "-m", "other"]);
        side.git(&["checkout", "-q", "main"]);
        let m = side.git(&["merge", "-q", "--no-ff", "-m", "merge", "other"]);
        assert_eq!(m.code, 0, "{m:?}");
    }
    for args in [
        &["diff-tree", "--always", "main~3"][..],
        &["diff-tree", "--always", "-r", "main~3"],
        &["diff-tree", "-u", "--patch-with-raw", "--always", "-m", "main~3"],
        &["diff-tree", "--always", "--no-commit-id", "main~3"],
        &["diff-tree", "--always", "HEAD"],
        &["diff-tree", "HEAD"],
        &["diff-tree", "main~3"],
        &["diff-tree", "--always", "--root", "main~3"],
        &["diff-tree", "--always", "-m", "HEAD"],
    ] {
        let want = s.git(args);
        assert_eq!(want.code, 0, "{args:?}: {want:?}");
        assert_eq!(z.git(args), want, "{args:?}");
    }
}
