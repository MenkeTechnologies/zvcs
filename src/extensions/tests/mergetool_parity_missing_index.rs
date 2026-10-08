//! `git mergetool` against stock git when the index file does not exist.
//!
//! `git diff --name-only --diff-filter=U` reads a missing index as an empty one, so the
//! script reaches `No files need merging` (after the `merge.tool` guidance on stderr)
//! and exits 0 — in a repository that has never staged anything, and under a
//! `GIT_INDEX_FILE` that names nothing.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn a_missing_index_has_nothing_to_merge() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("mergetool-index", stock);
    let run = |side: &twin_repo::Side| {
        let missing = side.root.join("no-such-index");
        let named = side.git_env(&[("GIT_INDEX_FILE", missing.to_str().unwrap())], &["mergetool"]);
        let _ = std::fs::remove_file(side.repo().join(".git/index"));
        let unstaged = side.git(&["mergetool"]);
        (named, unstaged)
    };
    let (want_named, want_unstaged) = run(&s);
    assert_eq!(want_named.stdout, "No files need merging\n", "stock: {want_named:?}");
    let (got_named, got_unstaged) = run(&z);
    assert_eq!(got_named, want_named);
    assert_eq!(got_unstaged, want_unstaged);
}

#[test]
fn a_missing_index_with_a_merge_rr_record_has_nothing_to_merge() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("mergetool-index-rr", stock);
    for side in [&s, &z] {
        side.write(".git/MERGE_RR", "");
        side.git(&["config", "rerere.enabled", "true"]);
        let _ = std::fs::remove_file(side.repo().join(".git/index"));
    }
    assert_eq!(z.git(&["mergetool"]), s.git(&["mergetool"]));
}
