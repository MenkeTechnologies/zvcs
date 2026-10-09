//! `git mv` re-stats each moved entry against the file at its new name
//! (`rename_index_entry_at()` -> `refresh_cache_entry()`). The rename changed the file's
//! ctime, so `ie_modified()` reads the content to prove it unchanged, and that read goes
//! through `index_fd()`, which looks up `core.bigFileThreshold`. A value `git_config_ulong()`
//! refuses therefore ends the command with `fatal: bad numeric config value ...` after the
//! file was renamed and before the index was written. A symlink target is compared without
//! the lookup, and a move whose content read never happens is not affected.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use std::time::Duration;

fn settle() {
    // Stock git compares stat times to the second (no `USE_NSEC`), so the rename only makes
    // the entry look stale once the clock has ticked past the second `add` recorded.
    std::thread::sleep(Duration::from_millis(1100));
}

#[test]
fn bad_threshold_dies_after_the_rename_and_leaves_the_index_alone() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("mv-big-file-threshold", stock);
    settle();
    let args = ["-c", "core.bigFileThreshold=bogus", "mv", "a", "moved"];
    let want = s.git(&args);
    assert_eq!(want.code, 128, "premise: stock dies: {want:?}");
    assert_eq!(z.git(&args), want);
    for probe in [&["status", "--porcelain"][..], &["ls-files", "--stage"]] {
        assert_eq!(z.git(probe), s.git(probe), "{probe:?}");
    }
}

#[test]
fn bad_threshold_from_a_file_names_it() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("mv-big-file-threshold-file", stock);
    for side in [&s, &z] {
        side.git(&["config", "core.bigFileThreshold", "12q"]);
    }
    settle();
    let args = ["mv", "a", "moved"];
    let want = s.git(&args);
    assert_eq!(want.code, 128, "premise: stock dies: {want:?}");
    assert_eq!(z.git(&args), want);
}

#[test]
fn symlink_and_valid_threshold_moves_are_unaffected() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("mv-big-file-threshold-ok", stock);
    for side in [&s, &z] {
        std::os::unix::fs::symlink("a", side.repo().join("lnk")).unwrap();
        side.git(&["add", "lnk"]);
        side.git(&["commit", "-q", "-m", "link"]);
    }
    settle();
    for args in [
        &["-c", "core.bigFileThreshold=bogus", "mv", "lnk", "lnk2"][..],
        &["-c", "core.bigFileThreshold=1k", "mv", "a", "moved"],
    ] {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
    }
    assert_eq!(z.git(&["status", "--porcelain"]), s.git(&["status", "--porcelain"]));
}
