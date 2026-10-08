//! `--orphan` meets the same pathspec refusals as `-b`.
//!
//! `cmd_checkout()` folds `--orphan` into `opts->new_branch` (builtin/checkout.c:1957-1962), so the
//! `parse_branchname_arg()` and `checkout_paths()` dies that guard `-b <name>` guard it too:
//! `--orphan=o -- f` is `'f' is not a commit and a branch 'o' cannot be created from it`,
//! `--orphan=o HEAD -- f` is `Cannot update paths and switch to branch 'o' at the same time.`, and
//! an operand before `--` that does not resolve is `invalid reference: <operand>`. zvcs wrote an
//! unborn branch and switched to it whatever followed `--`, and `-b` took the friendly message for
//! the operand before `--` instead of `invalid reference`.
//!
//! Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin.rs"]
mod twin;

use twin::Twin;

#[test]
fn orphan_beside_a_pathspec_is_refused_like_a_new_branch() {
    let Some(t) = Twin::new("orphan-paths") else { return };
    for args in [
        &["checkout", "--orphan=o", "--", "x"][..],
        &["checkout", "--orphan=o", "--", "no-such"],
        &["checkout", "--orphan=o", "no-such"],
        &["checkout", "--orphan=o", "main", "--", "x"],
        &["checkout", "--orphan=o", "main", "x"],
        &["checkout", "--orphan=o", "--", "x", "y"],
    ] {
        t.same(args);
    }
    let (stock, zvcs) = t.run_in("work", &["checkout", "--orphan=o", "--", "x"]);
    assert_eq!(stock.code, 128, "{stock:?}");
    assert!(stock.stderr.contains("'x' is not a commit and a branch 'o' cannot be created from it"), "{stock:?}");
    assert_eq!(zvcs, stock);
}

#[test]
fn an_operand_before_the_separator_must_be_a_revision() {
    let Some(t) = Twin::new("new-branch-dashdash") else { return };
    t.same(&["checkout", "-b", "nb", "no-such", "--"]);
    t.same(&["checkout", "-b", "nb", "no-such", "--", "x"]);
    t.same(&["checkout", "--orphan=o", "no-such", "--"]);
    t.same(&["checkout", "-B", "nb", "no-such", "--"]);
}

#[test]
fn a_bare_separator_still_creates_the_branch() {
    let Some(t) = Twin::new("new-branch-bare-dashdash") else { return };
    t.same(&["checkout", "-b", "nb", "--"]);
    t.same(&["checkout", "-B", "nb2", "main", "--"]);
    t.same(&["checkout", "--orphan", "o", "--"]);
    t.same(&["status", "--short", "--branch"]);
}
