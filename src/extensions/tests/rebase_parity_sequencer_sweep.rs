//! `git rebase` through the sequencer, compared with stock git step by step and
//! state file by state file (see `support/sequencer_diff.rs`).
//!
//! Each case pins one place zvcs used to part from git 2.56.0:
//!
//! * a `reword` and the last `squash`/`fixup -c` of a chain are committed by the
//!   `git commit` child `run_git_commit()` starts with
//!   `GIT_REFLOG_ACTION=<action> (<command>)` (sequencer.c:1143, :2301-2303), so
//!   `HEAD`'s reflog reads `rebase (reword): …` / `rebase (squash): …`, not
//!   `commit (amend): …`;
//! * a `fixup -c` writes no `message-fixup` (sequencer.c:2050-2060), so the end
//!   of its chain opens the editor on `SQUASH_MSG` rather than committing the
//!   message unseen;
//! * the end of a chain copies `message-squash` to `SQUASH_MSG` before its merge
//!   (sequencer.c:2440-2452), so a conflict there leaves `SQUASH_MSG` behind;
//! * `exec` checks the tree with `require_clean_work_tree()`, whose
//!   `error: cannot rebase: You have unstaged changes.` precedes the warning
//!   (sequencer.c:3796);
//! * `--show-current-patch` is a `git show REBASE_HEAD --` child, so at a `break`
//!   it is that child's `bad revision` and exit 1 (builtin/rebase.c:387-394);
//! * `--continue` of the apply backend refuses unmerged paths with
//!   `cmd_rebase()`'s stdout two-liner (builtin/rebase.c:1358-1380);
//! * `--edit-todo` of a sheet that does not parse keeps the old backup
//!   (rebase-interactive.c:119-133);
//! * a branch `--update-refs` will move is held like a checked-out one
//!   (branch.c:463-472), and a ref update that fails at the end reports the
//!   refusal and keeps the state directory (sequencer.c:4662-4666, :5343-5344);
//! * a `fixup -C` whose commit comes out identical to `HEAD` writes no reflog
//!   line, as the files backend skips a no-op update (refs/files-backend.c:2857-2866).

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/sequencer_diff.rs"]
mod sequencer_diff;

use sequencer_diff::{assert_same, run};
use stock_git::stock_git;

const FIVE: &str = "l1\\nl2\\nl3\\nl4\\nl5\\nl6\\nl7\\nl8\\n";

/// `topic` (t1, t2, t3) forked from `main` at B; `main` gained m1 and m2, which
/// change other lines than `topic` does.
fn lin() -> String {
    format!(
        "c a.txt '{FIVE}' A; c b.txt 'b\\n' B
git checkout -q -b topic
c t1.txt 't1\\n' t1; c a.txt 'L1\\nl2\\nl3\\nl4\\nl5\\nl6\\nl7\\nl8\\n' t2; c t3.txt 't3\\n' t3
git checkout -q main
c m1.txt 'm1\\n' m1; c a.txt 'l1\\nl2\\nl3\\nl4\\nl5\\nl6\\nl7\\nM8\\n' m2
git checkout -q topic
"
    )
}

/// As [`lin`], but m2 and t3 rewrite the same line of `a.txt`.
fn conf() -> String {
    format!(
        "c a.txt '{FIVE}' A; c b.txt 'b\\n' B
git checkout -q -b topic
c t1.txt 't1\\n' t1; c t2.txt 't2\\n' t2; c a.txt 'T1\\nl2\\nl3\\nl4\\nl5\\nl6\\nl7\\nl8\\n' t3
git checkout -q main
c m1.txt 'm1\\n' m1; c a.txt 'M1\\nl2\\nl3\\nl4\\nl5\\nl6\\nl7\\nl8\\n' m2
git checkout -q topic
"
    )
}

/// `part1` (p1) ⊂ `part2` (p2) ⊂ `topic` (p3) stacked on A; `main` gained m1.
fn stack() -> String {
    format!(
        "c a.txt '{FIVE}' A
git checkout -q -b part1; c p1.txt 'p1\\n' p1
git checkout -q -b part2; c p2.txt 'p2\\n' p2
git checkout -q -b topic; c p3.txt 'p3\\n' p3
git checkout -q main; c m1.txt 'm1\\n' m1; git checkout -q topic
"
    )
}

fn check(tag: &str, script: &str, steps: &[&str]) {
    let Some(stock) = stock_git() else {
        eprintln!("skipping: no stock git");
        return;
    };
    assert_same(&run(stock, tag, script, steps));
}

#[test]
fn reword_is_logged_as_rebase_reword() {
    check(
        "reword",
        &lin(),
        &["GIT_SEQUENCE_EDITOR=$ED/seq-reword2 GIT_EDITOR=$ED/msg-rw git rebase -i main"],
    );
    check(
        "reword-action",
        &lin(),
        &["GIT_REFLOG_ACTION=zzz GIT_SEQUENCE_EDITOR=$ED/seq-reword2 GIT_EDITOR=$ED/msg-rw git rebase -i main"],
    );
}

#[test]
fn final_squash_is_logged_as_rebase_squash() {
    check(
        "squash",
        &lin(),
        &["GIT_SEQUENCE_EDITOR=$ED/seq-squash3 GIT_EDITOR=$ED/msg-rw git rebase -i main"],
    );
}

#[test]
fn fixup_c_opens_the_editor_and_fixup_upper_c_does_not() {
    check(
        "fixup-c",
        &lin(),
        &["GIT_SEQUENCE_EDITOR=$ED/seq-fixupc3 GIT_EDITOR=$ED/msg-dump git rebase -i main"],
    );
    check(
        "fixup-upper-c",
        &lin(),
        &["GIT_SEQUENCE_EDITOR=$ED/seq-fixup-replace3 GIT_EDITOR=$ED/msg-dump git rebase -i main"],
    );
}

#[test]
fn conflicting_squash_leaves_squash_msg() {
    check(
        "squash-conflict",
        &conf(),
        &[
            "GIT_SEQUENCE_EDITOR=$ED/seq-squash3 GIT_EDITOR=$ED/msg-dump git rebase -i main",
            "git checkout --theirs a.txt && git add a.txt",
            "GIT_EDITOR=$ED/msg-dump git rebase --continue",
        ],
    );
}

#[test]
fn exec_that_dirties_the_tree_reports_require_clean_work_tree() {
    check(
        "exec-dirty",
        &lin(),
        &["GIT_SEQUENCE_EDITOR=$ED/seq-execdirty git rebase -i main", "git rebase --continue"],
    );
}

#[test]
fn show_current_patch_without_rebase_head() {
    check(
        "show-patch",
        &lin(),
        &["GIT_SEQUENCE_EDITOR=$ED/seq-break1 git rebase -i main", "git rebase --show-current-patch"],
    );
    check(
        "show-patch-edit",
        &lin(),
        &["GIT_SEQUENCE_EDITOR=$ED/seq-edit2 git rebase -i main", "git rebase --show-current-patch"],
    );
}

#[test]
fn apply_backend_continue_with_unmerged_paths() {
    check("apply-continue", &conf(), &["git rebase --apply main", "git rebase --continue"]);
}

#[test]
fn edit_todo_of_a_bad_sheet_keeps_the_backup() {
    check(
        "edit-todo-bad",
        &lin(),
        &[
            "GIT_SEQUENCE_EDITOR=$ED/seq-bogus git rebase -i main",
            "git rebase --edit-todo",
            "git status",
        ],
    );
}

#[test]
fn update_refs_holds_its_branches_and_keeps_state_on_failure() {
    check(
        "update-refs-held",
        &stack(),
        &[
            "GIT_SEQUENCE_EDITOR=$ED/seq-edit2 git rebase -i --update-refs main",
            "git branch -f part1 main",
            "git rebase --continue",
        ],
    );
    check(
        "update-refs-failed",
        &stack(),
        &[
            "GIT_SEQUENCE_EDITOR=$ED/seq-edit2 git rebase -i --update-refs main",
            "git branch -m part1 renamed",
            "git rebase --continue",
        ],
    );
}

#[test]
fn identical_fixup_writes_no_reflog_line() {
    check(
        "reword-fixup-noop",
        &lin(),
        &["git commit --fixup=reword:HEAD~1", "GIT_EDITOR=$ED/msg-dump git rebase -i --autosquash main"],
    );
}
