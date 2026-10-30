//! `git rebase -i`, `--exec` and `--autosquash` against stock git when `<onto>` already descends
//! from `HEAD`: they clear `allow_preemptive_ff` (builtin/rebase.c:1476-1481), so
//! `can_fast_forward()` is never asked and `is_merge()` goes to `run_specific_rebase()`
//! (:1872-1873). `complete_action()` then hands the sequence editor a `noop` sheet, and a failing
//! editor fails the rebase, rather than the branch being fast-forwarded without one.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use twin_repo::Side;

/// `behind` sits at `main~1`, so `main` is a proper descendant of it.
fn behind(side: &Side) {
    side.git(&["checkout", "-q", "-b", "behind", "main~1"]);
    // The editor copies the sheet it was given out of `.git`, for comparison.
    side.write(".git/info/exclude", "sheet\n");
}

fn both(label: &str) -> Option<(Side, Side)> {
    let stock = stock_git::stock_git()?;
    let (s, z) = twin_repo::pair(label, stock);
    behind(&s);
    behind(&z);
    Some((s, z))
}

fn check(s: &Side, z: &Side, env: &[(&str, &str)], args: &[&str]) {
    assert_eq!(z.git_env(env, args), s.git_env(env, args), "{env:?} {args:?}");
    assert_eq!(z.read("sheet"), s.read("sheet"), "sheet seen by {env:?} {args:?}");
    for probe in [&["status", "--porcelain"][..], &["for-each-ref"], &["reflog", "list"], &["log", "--format=%H", "-1"]] {
        assert_eq!(z.git(probe), s.git(probe), "{probe:?} after {args:?}");
    }
}

#[test]
fn a_failing_sequence_editor_fails_an_interactive_rebase_that_could_fast_forward() {
    for (label, args) in [
        ("rebase-noop-i", &["rebase", "-i", "main"][..]),
        ("rebase-noop-i-rcp", &["rebase", "-i", "--reapply-cherry-picks", "main"]),
        ("rebase-noop-x", &["rebase", "-x", "true", "main"]),
    ] {
        let Some((s, z)) = both(label) else {
            return;
        };
        check(&s, &z, &[("GIT_SEQUENCE_EDITOR", "false")], args);
    }
}

#[test]
fn the_editor_is_handed_a_noop_sheet_and_the_rebase_then_finishes() {
    for (label, args) in [
        ("rebase-noop-sheet-i", &["rebase", "-i", "main"][..]),
        ("rebase-noop-sheet-iv", &["rebase", "-v", "-i", "main"]),
        ("rebase-noop-sheet-x", &["rebase", "-x", "true", "main"]),
        ("rebase-noop-sheet-xq", &["rebase", "-q", "-x", "true", "main"]),
        ("rebase-noop-sheet-as", &["rebase", "--autosquash", "main"]),
        ("rebase-noop-sheet-plain", &["rebase", "main"]),
    ] {
        let Some((s, z)) = both(label) else {
            return;
        };
        check(&s, &z, &[("GIT_SEQUENCE_EDITOR", "cp .git/rebase-merge/git-rebase-todo sheet; :")], args);
    }
}
