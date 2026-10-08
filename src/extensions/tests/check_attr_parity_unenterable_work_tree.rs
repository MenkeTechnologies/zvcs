//! `check-attr` enters its work tree before it parses a single option.
//!
//! `cmd_check_attr()` opens with `if (!is_bare_repository()) setup_work_tree();`
//! (builtin/check-attr.c:110-111), and `setup_work_tree()` dies with `this operation must be run
//! in a work tree` when the tree it was handed cannot be entered (setup.c:503-505). Typed inside
//! `.git/hooks`, `--work-tree=src` names `.git/hooks/src`, which is not there, so git refuses and
//! prints nothing. zvcs only refused a repository with no work tree at all and went on to answer
//! `x: eol: unspecified`.
//!
//! Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin.rs"]
mod twin;

use twin::Twin;

#[test]
fn a_work_tree_that_cannot_be_entered_refuses_before_any_answer() {
    let Some(t) = Twin::new("check-attr-wt") else { return };
    let (stock, zvcs) = t.run_in("work/.git/hooks", &["--work-tree=src", "check-attr", "eol", "x"]);
    assert_eq!(stock.code, 128, "{stock:?}");
    assert_eq!(zvcs, stock);
    t.same_in("work/.git/hooks", &["--work-tree=src", "check-attr", "-a", "--cached", "x"]);
    t.same_in("work/.git/hooks", &["--work-tree=no-such", "check-attr", "--bogus-option"]);
}

#[test]
fn an_enterable_work_tree_still_answers() {
    let Some(t) = Twin::new("check-attr-wt-ok") else { return };
    t.mkdir("work/src");
    let (stock, zvcs) = t.run_in("work", &["--work-tree=src", "check-attr", "eol", "x"]);
    assert_eq!(stock.code, 0, "{stock:?}");
    assert_eq!(zvcs, stock);
}
