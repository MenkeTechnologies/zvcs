//! `--ours`/`--theirs` together with `-m`/`--conflict` is refused by `checkout_main()`
//! before any other option combination or pathspec is looked at:
//!
//! ```c
//! if (1 < !!opts->writeout_stage + !!opts->force + !!opts->merge)
//!         die(_("git checkout: --ours/--theirs, --force and --merge are incompatible when\n"
//!               "checking out of the index."));
//! ```
//!
//! `restore` has no `--force`, so the pair is the stage pick plus the merge request. Its
//! other refusals (`--staged`, `--source`, `--ignore-unmerged`, an unmatched pathspec)
//! used to answer first, or the command succeeded.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn stage_pick_with_merge_request_is_refused_before_every_other_check() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("restore-ours-merge", stock);
    for args in [
        &["restore", "--ours", "-m", "--", "nosuch"][..],
        &["restore", "--theirs", "--merge", "a"],
        &["restore", "-2", "--conflict=diff3", "a"],
        &["restore", "--ours", "-m", "--staged", "a"],
        &["restore", "--ours", "-m", "--source=HEAD", "a"],
        &["restore", "--ours", "-m", "--ignore-unmerged", "a"],
        &["restore", "--ours", "-m", "-p", "a"],
        &["restore", "--no-merge", "-3", "-m", "--ours", "--", "nosuch"],
    ] {
        let want = s.git(args);
        assert_eq!(want.code, 128, "premise: stock refuses {args:?}: {want:?}");
        assert_eq!(z.git(args), want, "{args:?}");
    }
}

#[test]
fn stage_pick_alone_or_merge_alone_is_not_refused() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("restore-ours-merge-ok", stock);
    for args in [&["restore", "--ours", "a"][..], &["restore", "-m", "a"], &["restore", "--ours", "--no-merge", "a"]] {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
    }
}
