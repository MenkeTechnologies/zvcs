//! `git diff` on an unmerged path renders the combined patch through `show_combined_diff()`,
//! and what that route does and does not honour:
//!
//! * `show_combined_header()` prints `opt->a_prefix` / `opt->b_prefix` on the `---` / `+++`
//!   lines (combine-diff.c:931-932), so `--src-prefix`, `--dst-prefix` and `--no-prefix`
//!   reach them. zvcs hard-coded `a/` and `b/`.
//! * `run_diff_files()` emits the combined section and `continue`s before `diff_unmerge()` /
//!   `diff_change()` (diff-lib.c:210-214), so no pair is queued, `has_changes` stays clear and
//!   `--exit-code` exits 0 for a patch that holds nothing but combined sections. The
//!   stat/raw/name formats take the ordinary route and exit 1. zvcs exited 1 throughout.
//! * `-R` is not looked at by the combined route: the parents keep their order.
//!
//! Expectations come from stock git (`support/stock_git.rs`) in an identical repository.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

fn conflicted(label: &str, stock: &str) -> (twin_repo::Side, twin_repo::Side) {
    let (s, z) = twin_repo::pair(label, stock);
    for side in [&s, &z] {
        let merge = side.git(&["merge", "side"]);
        assert_eq!(merge.code, 1, "{merge:?}");
    }
    (s, z)
}

#[test]
fn prefix_options_reach_the_combined_header() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = conflicted("diff-cc-prefix", stock);
    for args in [
        &["diff", "--src-prefix=X/"][..],
        &["diff", "--dst-prefix=Y/"],
        &["diff", "--no-prefix"],
        &["diff", "--src-prefix=X/", "--dst-prefix=Y/", "--cc"],
        &["diff", "-c", "--src-prefix=X/"],
        &["diff", "-R", "--src-prefix=X/"],
    ] {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
    }
}

#[test]
fn exit_code_follows_whether_a_pair_was_queued() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = conflicted("diff-cc-exit", stock);
    for fmt in [
        &[][..],
        &["-p"],
        &["-w"],
        &["-U0"],
        &["--cc"],
        &["-c"],
        &["--stat"],
        &["--numstat"],
        &["--name-only"],
        &["--name-status"],
        &["--raw"],
        &["--ours"],
        &["-p", "--name-only"],
    ] {
        let mut args = vec!["diff", "--exit-code"];
        args.extend_from_slice(fmt);
        assert_eq!(z.git(&args), s.git(&args), "{args:?}");
    }
}

#[test]
fn a_second_changed_path_still_exits_1() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = conflicted("diff-cc-exit-mixed", stock);
    for side in [&s, &z] {
        side.write("other", "x\n");
        side.git(&["add", "other"]);
        side.write("other", "y\n");
    }
    for args in [&["diff", "--exit-code"][..], &["diff", "--exit-code", "-w"]] {
        let (want, got) = (s.git(args), z.git(args));
        assert_eq!(want.code, 1, "{args:?}: {want:?}");
        assert_eq!(got, want, "{args:?}");
    }
}
