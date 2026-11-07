//! The order `git commit` refuses its options in, compared with stock git (see
//! `support/sequencer_diff.rs`).
//!
//! `cmd_commit()` runs under `RUN_SETUP | NEED_WORK_TREE` (git.c:560), reads the
//! status config, `determine_whence()` and `HEAD` (builtin/commit.c:1808-1819),
//! and only then `parse_and_validate_options()` (:1309-1425): `-z`/`--long`,
//! the `--author` nickname and `--reset-author`, `--amend` without a `HEAD` or
//! during an operation, `--squash` with `--fixup`, the message sources, the
//! `-C`/`-c` lookup, the staging modes, `--fixup=reword:`'s
//! `check_fixup_reword_options()` and an unknown `--fixup` suboption, `-u<mode>`,
//! and `-a` with paths, in that order. `get_cleanup_mode()` follows (:1830-1834),
//! then `prepare_index()` (:351-440): the negative `-U` values, the
//! command-line pathspec's magic, the `--pathspec-from-file` conflicts, "No paths
//! with --include/--only" and the `-U` that needs `--patch`.

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/sequencer_diff.rs"]
mod sequencer_diff;

use sequencer_diff::{assert_same, run};
use stock_git::stock_git;

const HISTORY: &str = "c a.txt 'a\\n' A
c a.txt 'a2\\n' B
printf 'a3\\n' >a.txt
";

fn check(tag: &str, script: &str, args: &str) {
    let Some(stock) = stock_git() else {
        eprintln!("skipping: no stock git");
        return;
    };
    assert_same(&run(stock, tag, script, &[&format!("git commit {args}")]));
}

#[test]
fn fixup_reword_is_judged_before_paths_with_all() {
    for (tag, args) in [
        ("reword-all-path", "--fixup=reword:HEAD -a a.txt"),
        ("reword-all-bad-magic", "--fixup=reword:HEAD -a ':(bogus)x'"),
        ("reword-missing-commit", "--fixup=reword:nosuch -a x"),
        ("reword-untracked-mode", "--fixup=reword:HEAD -u=bad x"),
        ("reword-cleanup", "--fixup=reword:HEAD --cleanup=bad x"),
        ("reword-all-from-file", "--fixup=reword:HEAD -a --pathspec-from-file=nonexist"),
        ("unknown-suboption", "--fixup=bogus:HEAD -a x"),
        ("squash-and-fixup", "--squash=HEAD --fixup=HEAD -a x"),
        ("reuse-missing", "-C nosuch -a x"),
        ("reuse-missing-dry-run", "-C nosuch --dry-run"),
    ] {
        check(tag, HISTORY, args);
    }
}

#[test]
fn cleanup_and_pathspec_checks_follow_the_option_checks() {
    for (tag, args) in [
        ("all-path-cleanup", "-a --cleanup=bad x"),
        ("all-untracked-mode", "-a -u=bad x"),
        ("cleanup-untracked-mode", "--cleanup=bad -u=bad"),
        ("all-path-from-file", "-a --pathspec-from-file=f x"),
        ("patch-from-file", "-p --pathspec-from-file=f x"),
        ("path-from-file", "--pathspec-from-file=f x"),
        ("file-nul", "--pathspec-file-nul x"),
        ("bad-magic", "-m x ':(bogus)x'"),
        ("unified-needs-patch", "-U3 -m x"),
        ("negative-unified-only", "-U-3 -o"),
        ("include-no-paths", "-i"),
    ] {
        check(tag, HISTORY, args);
    }
    if let Some(stock) = stock_git() {
        assert_same(&run(stock, "config-cleanup", HISTORY, &["git -c commit.cleanup=bad commit -a x"]));
    }
}

#[test]
fn unborn_head_and_amend() {
    let unborn = "printf 'a\\n' >a.txt\n";
    for (tag, args) in [
        ("amend-all-path", "--amend -a x"),
        ("amend-reword", "--amend --fixup=reword:HEAD"),
        ("amend-squash-fixup", "--amend --squash=HEAD --fixup=HEAD"),
        ("amend-modes", "--amend -i -o"),
        ("amend-cleanup", "--amend --cleanup=bad"),
        ("amend-dry-run", "--dry-run --amend"),
        ("reword-all-path", "--fixup=reword:HEAD -a x"),
        ("reuse-all-path", "-C HEAD -a x"),
    ] {
        check(&format!("unborn-{tag}"), unborn, args);
    }
}

#[test]
fn outside_a_repository_nothing_is_judged() {
    let Some(stock) = stock_git() else {
        eprintln!("skipping: no stock git");
        return;
    };
    for (tag, args) in [("all-path", "-a x"), ("reword", "--fixup=reword:HEAD -a x"), ("cleanup", "--cleanup=bad")] {
        let step = format!("mkdir -p ../norepo && cd ../norepo && GIT_CEILING_DIRECTORIES=\"$(cd .. && pwd)\" git commit {args}");
        assert_same(&run(stock, &format!("norepo-{tag}"), "", &[&step]));
    }
}
