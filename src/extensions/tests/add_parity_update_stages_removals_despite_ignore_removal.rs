//! `git add -u --ignore-removal` (and `stage`) against stock git: `-u` stages the removal of
//! every tracked path that is gone, whatever `--ignore-removal` / `--no-all` say — also when
//! `--work-tree` names a directory that holds none of the tracked paths.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn update_stages_removals_whatever_ignore_removal_says() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("add-u-ignore-removal", stock);
    for side in [&s, &z] {
        side.write("gone.txt", "gone\n");
        side.write("kept.txt", "kept\n");
        side.git(&["add", "."]);
        side.git(&["commit", "-q", "-m", "files"]);
    }
    let status = |side: &twin_repo::Side| side.git(&["status", "--porcelain"]);
    for args in [
        &["add", "-u", "--ignore-removal"][..],
        &["add", "--ignore-removal", "-u"],
        &["add", "-u", "--no-all"],
        &["stage", "--no-all", "--update"],
        &["add", "--ignore-removal", "."],
        &["add", "-u"],
    ] {
        for side in [&s, &z] {
            side.git(&["reset", "-q", "--hard"]);
            let _ = std::fs::remove_file(side.repo().join("gone.txt"));
            side.write("kept.txt", "kept, edited\n");
        }
        assert_eq!(z.git(args), s.git(args), "{args:?}");
        assert_eq!(status(&z), status(&s), "status after {args:?}");
    }
}

#[test]
fn update_with_a_work_tree_that_holds_no_tracked_path_stages_every_removal() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("add-u-work-tree", stock);
    for side in [&s, &z] {
        side.write("src/lib.rs", "lib\n");
        side.git(&["add", "."]);
        side.git(&["commit", "-q", "-m", "src"]);
    }
    let args = ["--work-tree=src", "stage", "--ignore-removal", "-f", "--update"];
    assert_eq!(z.git(&args), s.git(&args));
    assert_eq!(z.git(&["status", "--porcelain"]), s.git(&["status", "--porcelain"]));
}
