//! `git ls-files --error-unmatch`: `do_match_pathspec()` sets `seen[i]` for *every*
//! positive element a path hits, also when a later exclusion then removes the path, so
//! an element that only ever matched excluded paths is not reported as unmatched.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn element_hit_only_by_excluded_paths_is_not_unmatched() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("ls-files-unmatch-excluded", stock);
    for side in [&s, &z] {
        side.write("src/lib.rs", "lib\n");
        side.write("README.md", "readme\n");
        side.git(&["add", "."]);
        side.git(&["commit", "-q", "-m", "src"]);
    }
    for args in [
        &["ls-files", "--error-unmatch", ":/src", ":!src"][..],
        &["ls-files", ":(exclude,icase)*.MD", "--error-unmatch", ":/src", ":!src"],
        &["ls-files", "--error-unmatch", "src", "src/lib.rs", ":!src/lib.rs"],
        &["ls-files", "--error-unmatch", "src", "nosuch", ":!src"],
        &["ls-files", "--error-unmatch", "a", ":!a", "README.md"],
    ] {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
    }
}
