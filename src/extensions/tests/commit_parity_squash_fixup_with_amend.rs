//! `git commit --squash` / `--fixup` next to `-c`/`-C`/`--amend`/`--only`,
//! compared with stock git (see `support/sequencer_diff.rs`).
//!
//! `prepare_to_commit()` writes the `squash! <subject>` line first and then
//! whatever `use_message` names — `-c`/`-C`, or the `HEAD` an `--amend` without
//! `--fixup` implies — whole, from the end of the commit headers
//! (builtin/commit.c:784-824, :1351-1354); only when `use_message` *is* the
//! squash target does the line shrink to `squash! ` (:789-790). The same rule
//! makes `--amend --fixup=<c>` compose a new author rather than keep `HEAD`'s.
//! `--fixup=amend:` sets `allow_empty` while it is parsed, so `--only` with no
//! paths is accepted (builtin/commit.c:387-390, :1395-1398), and its `-m`
//! refusal comes from `prepare_to_commit()`, after `prepare_index()` has
//! reported an unmatched pathspec (:851-853).

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/sequencer_diff.rs"]
mod sequencer_diff;

use sequencer_diff::{assert_same, run};
use stock_git::stock_git;

/// `main` with A and B, `topic` adding t1, t2 (with a body) and t3, and a staged
/// change to `a.txt`.
const LIN: &str = "c a.txt 'l1\\nl2\\n' A; c b.txt 'b\\n' B
git checkout -q -b topic
c t1.txt 't1\\n' t1
printf 't2\\n' >t2.txt; git add t2.txt; tick; git commit -q -m t2 -m 'body of t2'
c t3.txt 't3\\n' t3
echo chg >>a.txt; git add a.txt
";

fn check(tag: &str, steps: &[&str]) {
    let Some(stock) = stock_git() else {
        eprintln!("skipping: no stock git");
        return;
    };
    assert_same(&run(stock, tag, LIN, steps));
}

#[test]
fn squash_takes_the_reused_message_whole() {
    check("squash-c", &["GIT_EDITOR=$ED/msg-dump git commit --squash=HEAD~1 -c HEAD"]);
    check("squash-upper-c", &["git commit --squash=HEAD~2 -C HEAD~1"]);
    check("squash-same", &["git commit --squash=HEAD~1 -C HEAD~1"]);
}

#[test]
fn squash_with_amend_takes_head_message() {
    check("squash-amend", &["GIT_EDITOR=$ED/msg-dump git commit --squash=HEAD~1 --amend"]);
    check("squash-amend-no-edit", &["git commit --squash=HEAD~1 --amend --no-edit"]);
}

#[test]
fn fixup_with_amend_composes_a_new_author() {
    check("fixup-amend", &["git commit --fixup=HEAD~1 --amend"]);
    check("fixup-amend-amend", &["git commit --fixup=amend:HEAD~1 --amend --no-edit"]);
}

#[test]
fn fixup_amend_and_only() {
    check("fixup-amend-only", &["GIT_EDITOR=$ED/msg-dump git commit --fixup=amend:HEAD~1 --only"]);
    check("fixup-only", &["git commit --fixup=HEAD~1 --only"]);
    check("fixup-amend-fixup-only", &["git commit --fixup=HEAD~1 --amend --only"]);
}

#[test]
fn fixup_amend_message_refusal_follows_pathspec() {
    check("fixup-amend-m-path", &["git commit --fixup=amend:HEAD~1 -m x nosuchpath"]);
    check("fixup-amend-m", &["git commit --fixup=amend:HEAD~1 -m x"]);
}
