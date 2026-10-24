//! `git diff` on an unmerged path in every output format, against stock git:
//!
//! * `run_diff_files()` hands a path holding stages #2 and #3 to `show_combined_diff()`
//!   whenever `revs->combine_merges` is set — by `-c`/`--cc`, or by `builtin_diff_files()`
//!   for the patch format (builtin/diff.c:280-282) — and queues no pair for it
//!   (diff-lib.c:211-215). `show_combined_diff()` prints the `::` raw record when a
//!   raw/name format is asked for, else the combined patch, else nothing
//!   (combine-diff.c:1284-1296), and it prints it during the index walk, ahead of
//!   everything `diff_flush()` makes of the queue. zvcs printed the `U` and stage-#2
//!   records under `--patch-with-raw`, refused `--cc --raw`, and sorted the combined
//!   patch in among the ordinary ones.
//! * A conflicted file removed from the worktree is `deleted_file:` there
//!   (combine-diff.c:1111-1116): the header and no hunks. Without the combined diff its
//!   stage pair is the `diff_addremove('-')` deletion (diff-lib.c:252-262).
//! * `show_dirstat()` charges a `U` pair the size of its one valid side (diff.c:3414-3426).
//!
//! Expectations come from stock git (`support/stock_git.rs`) in an identical repository.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

/// `merge side` conflicted on `a`, plus `0first`, a modified path that sorts ahead of it.
fn conflicted(label: &str, stock: &str) -> (twin_repo::Side, twin_repo::Side) {
    let (s, z) = twin_repo::pair(label, stock);
    for side in [&s, &z] {
        let merge = side.git(&["merge", "side"]);
        assert_eq!(merge.code, 1, "{merge:?}");
        side.write("0first", "x\n");
        side.git(&["add", "0first"]);
        side.write("0first", "y\n");
    }
    (s, z)
}

#[test]
fn combined_path_follows_the_format_and_prints_first() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = conflicted("diff-cc-formats", stock);
    for args in [
        &["diff"][..],
        &["diff", "--patch-with-raw"],
        &["diff", "--raw", "-p"],
        &["diff", "-p", "--stat"],
        &["diff", "--patch-with-stat"],
        &["diff", "--numstat", "-p"],
        &["diff", "-z", "--patch-with-raw"],
        &["diff", "--cc", "--raw"],
        &["diff", "--cc", "--stat"],
        &["diff", "--cc", "--name-only"],
        &["diff", "-c", "--name-status"],
        &["diff", "--cc", "--numstat"],
        &["diff", "--cc", "--theirs", "--raw"],
        &["diff", "--cc", "--exit-code", "--raw"],
        &["diff", "--line-prefix=>", "--raw", "-p"],
    ] {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
    }
}

#[test]
fn removed_conflicted_file() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = conflicted("diff-cc-removed", stock);
    for side in [&s, &z] {
        std::fs::remove_file(side.repo().join("a")).unwrap();
    }
    for args in [
        &["diff"][..],
        &["diff", "--raw"],
        &["diff", "--patch-with-raw"],
        &["diff", "--name-status"],
        &["diff", "--ours"],
        &["diff", "--base", "-p"],
    ] {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
    }
}

#[test]
fn dirstat_charges_the_unmerged_pair() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = conflicted("diff-cc-dirstat", stock);
    for args in [
        &["diff", "--dirstat=0"][..],
        &["diff", "--dirstat=files,0"],
        &["diff", "--cached", "--dirstat=0"],
    ] {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
    }
}
