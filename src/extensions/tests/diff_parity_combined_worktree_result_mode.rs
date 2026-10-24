//! The worktree result of a combined `git diff` carries two modes, against stock git:
//!
//! * `show_raw_diff()` prints `elem->mode` as it was built — `ce_mode_from_stat()`'s
//!   answer from `run_diff_files()` (diff-lib.c:157-167) or `get_stat_data()`
//!   (diff-lib.c:324-343) — while `show_patch_diff()` re-reads the file and takes
//!   `canon_mode()` of its own `lstat()` (combine-diff.c:1079). Under
//!   `core.fileMode=false` an executable file is `100644` in `-c --raw` and `100755`
//!   on the patch's `mode` line.
//! * A conflicted path replaced by a directory is removed to `check_removed()`, so its
//!   mode is `0`, but `lstat()` finds it in `show_patch_diff()`: the `S_ISDIR` branch
//!   reads `grab_blob()` of the null id, an empty result rather than a deleted one, so
//!   the `deleted file` header is followed by the hunk that empties the file
//!   (combine-diff.c:1059-1068). A repository standing there is a gitlink, read as its
//!   `Subproject commit` line.
//!
//! Expectations come from stock git (`support/stock_git.rs`) in an identical repository.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use std::os::unix::fs::PermissionsExt;

/// `merge side` conflicted on `a`.
fn conflicted(label: &str, stock: &str) -> (twin_repo::Side, twin_repo::Side) {
    let (s, z) = twin_repo::pair(label, stock);
    for side in [&s, &z] {
        let merge = side.git(&["merge", "side"]);
        assert_eq!(merge.code, 1, "{merge:?}");
    }
    (s, z)
}

const FORMS: &[&[&str]] = &[
    &["diff"],
    &["diff", "--cc"],
    &["diff", "-c"],
    &["diff", "-c", "--raw"],
    &["diff", "--cc", "--raw"],
    &["diff", "--cc", "--name-status"],
    &["diff", "HEAD", "--cc"],
    &["diff", "HEAD", "-c", "--raw"],
    &["diff", "HEAD", "--cc", "--name-status"],
];

#[test]
fn file_mode_off_splits_raw_and_patch_modes() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = conflicted("diff-cc-wt-mode", stock);
    for side in [&s, &z] {
        side.git(&["config", "core.fileMode", "false"]);
        let path = side.repo().join("a");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    for args in FORMS {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
    }
    // With the bit trusted both renderers agree on 100755.
    for side in [&s, &z] {
        side.git(&["config", "core.fileMode", "true"]);
    }
    for args in FORMS {
        assert_eq!(z.git(args), s.git(args), "fileMode=true {args:?}");
    }
}

#[test]
fn conflicted_path_replaced_by_a_directory() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = conflicted("diff-cc-wt-dir", stock);
    for side in [&s, &z] {
        std::fs::remove_file(side.repo().join("a")).unwrap();
        side.write("a/inner", "x\n");
    }
    for args in FORMS {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
    }
}

#[test]
fn conflicted_path_replaced_by_a_repository() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = conflicted("diff-cc-wt-gitlink", stock);
    for side in [&s, &z] {
        std::fs::remove_file(side.repo().join("a")).unwrap();
        let sub = side.repo().join("a");
        let init = side.run_in(&side.repo(), &[], &["init", "-q", "-b", "main", "a"]);
        assert_eq!(init.code, 0, "{init:?}");
        std::fs::write(sub.join("z"), "z\n").unwrap();
        side.run_in(&sub, &[], &["add", "z"]);
        let commit = side.run_in(&sub, &[], &["commit", "-q", "-m", "z"]);
        assert_eq!(commit.code, 0, "{commit:?}");
    }
    for args in FORMS {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
    }
}
