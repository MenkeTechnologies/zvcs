//! The revision options `cherry-pick` and `revert` hand to `setup_revisions()`
//! (`PARSE_OPT_KEEP_UNKNOWN_OPT`, builtin/revert.c:174), compared with stock git
//! (see `support/sequencer_diff.rs`).
//!
//! `--stdin` reads revisions with flags of its own (revision.c:2975-3023);
//! `--no-walk[=sorted|unsorted]` and `--do-walk` set `revs->no_walk`
//! (revision.c:2946-2961), which an uninteresting operand clears again;
//! `--max-count`, `-<n>` and `-n<n>` clear it too and, like `--skip`, cut what
//! `get_revision()` hands back, before `--reverse` and the pick's own reversal
//! (revision.c:2373-2410, sequencer.c:5479-5484). A value attached to an option
//! that takes none is parse-options' `takes no value` error at 129
//! (parse-options.c:138-143). An empty pick under `-e` still goes to the
//! `git commit -e` child, whose template lands in `COMMIT_EDITMSG` before it
//! refuses (sequencer.c:2544-2548).

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/sequencer_diff.rs"]
mod sequencer_diff;

use sequencer_diff::{assert_same, run};
use stock_git::stock_git;

/// `main`: A, B, C (both rewriting line 1 of `a.txt`), D. `other` from A: O1
/// (rewriting line 1 too), O2 (adds `c.txt`), O3 (rewrites line 8).
const CPC: &str = "c a.txt 'l1\\nl2\\nl3\\nl4\\nl5\\nl6\\nl7\\nl8\\n' A
c a.txt 'X1\\nl2\\nl3\\nl4\\nl5\\nl6\\nl7\\nl8\\n' B
c a.txt 'Y1\\nl2\\nl3\\nl4\\nl5\\nl6\\nl7\\nl8\\n' C
c b.txt 'b\\n' D
git checkout -q -b other main~3
c a.txt 'Z1\\nl2\\nl3\\nl4\\nl5\\nl6\\nl7\\nl8\\n' O1
c c.txt 'c\\n' O2
c a.txt 'Z1\\nl2\\nl3\\nl4\\nl5\\nl6\\nl7\\nZ8\\n' O3
git checkout -q main
";

/// `main` has a commit adding `x.txt` that `topic~2` adds identically.
const CHERRY: &str = "c a.txt 'a\\n' A
git checkout -q -b topic
c x.txt 'x\\n' t1; c y.txt 'y\\n' t2; c z.txt 'z\\n' t3
git checkout -q main
c x.txt 'x\\n' m0; c m1.txt 'm1\\n' m1
";

fn check(tag: &str, script: &str, steps: &[&str]) {
    let Some(stock) = stock_git() else {
        eprintln!("skipping: no stock git");
        return;
    };
    assert_same(&run(stock, tag, script, steps));
}

fn both(tag: &str, args: &str) {
    for cmd in ["cherry-pick", "revert"] {
        check(&format!("{tag}-{cmd}"), CPC, &[&format!("git {cmd} {args}")]);
    }
}

#[test]
fn stdin_revisions() {
    both("stdin-empty", "--stdin </dev/null");
    for (tag, input) in [
        ("stdin-one", "other~1"),
        ("stdin-range", "main..other"),
        ("stdin-two", "other~1\\nother"),
        ("stdin-not", "--not\\nmain\\n\\nother"),
        ("stdin-option", "--bogus"),
        ("stdin-bad", "nosuch"),
    ] {
        both(tag, &format!("--stdin <<EOF\n$(printf '{input}')\nEOF"));
    }
    both("stdin-twice", "--stdin --stdin </dev/null");
    both("stdin-after", "other --stdin <<EOF\n^main\nEOF");
}

#[test]
fn counts_and_reverse() {
    for (tag, args) in [
        ("n2", "-2 other"),
        ("n1", "-1 other"),
        ("max-count", "--max-count=2 other"),
        ("max-count-sep", "--max-count 1 other"),
        ("n0", "-0 other"),
        ("bad-count", "-1x other"),
        ("bad-max-count", "--max-count=x other"),
        ("reverse-range", "--reverse main..other"),
        ("reverse-list", "--reverse other~1 other"),
        ("n2-reverse", "-2 --reverse other"),
    ] {
        both(tag, args);
    }
}

#[test]
fn no_walk_and_do_walk() {
    for (tag, args) in [
        ("no-walk", "--no-walk other~1"),
        ("no-walk-range", "--no-walk main..other"),
        ("range-no-walk", "main..other --no-walk"),
        ("no-walk-sorted", "--no-walk=sorted other other~1"),
        ("no-walk-unsorted", "--no-walk=unsorted other other~1"),
        ("no-walk-bogus", "--no-walk=bogus other"),
        ("do-walk", "--do-walk other~1"),
        ("end-of-options", "--end-of-options other"),
    ] {
        both(tag, args);
    }
}

#[test]
fn attached_value_on_an_option_without_one() {
    for (tag, args) in [
        ("commit", "--commit=1 other"),
        ("edit", "--edit=1 other"),
        ("no-edit", "--no-edit=1 other"),
        ("skip", "--skip=1 other"),
        ("no-strategy", "--no-strategy=x other"),
        ("no-cleanup", "--no-cleanup=x other"),
    ] {
        both(tag, args);
    }
    check("ff", CPC, &["git cherry-pick --ff=1 other"]);
    check("reference", CPC, &["git revert --reference=1 other"]);
}

#[test]
fn edit_of_an_empty_pick_writes_the_template() {
    check("edit-empty", CHERRY, &["git cherry-pick -e topic~2"]);
    check("edit-empty-skip", CHERRY, &["git cherry-pick -e topic~2", "git cherry-pick --skip"]);
}
