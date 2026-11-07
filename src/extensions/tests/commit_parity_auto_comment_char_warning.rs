//! `core.commentChar=auto` is deprecated, and the five builtins that arm the
//! check say so before anything else.
//!
//! ```c
//! warn_on_auto_comment_char = true;
//! prepare_repo_settings(the_repository);
//! ```
//!
//! (builtin/commit.c:1802-1805, and the same in `cmd_merge()`, `cmd_rebase()`,
//! `cmd_revert()` and `cmd_cherry_pick()`.) The first repository config read
//! then runs `check_auto_comment_char_config()` (config.c:2255-2278): when the
//! last `core.commentChar`/`core.commentString` value is exactly `auto` it warns
//! `Support for '<key>=auto' is deprecated and will be removed in Git 3.0`, sets
//! `GIT_AUTO_COMMENT_CHAR_CONFIG_WARNING_GIVEN` so no child repeats it, and, when
//! that value came from a file, advises how to unset every file's setting (with
//! `--all` for a key a file holds twice) and how to set a custom string in the
//! last file. zvcs printed nothing.
//!
//! Expectations come from stock git (`support/stock_git.rs`, 2.52 or later, the
//! release that added the warning) in an identical repository.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

/// Run `setup` on both sides, then compare every `cases` invocation.
fn compare(label: &str, setup: &[&[&str]], cases: &[(&[(&str, &str)], &[&str])]) {
    let Some(stock) = stock_git::stock_git_at_least((2, 52, 0)) else { return };
    let (s, z) = twin_repo::pair(label, stock);
    for side in [&s, &z] {
        side.write("b", "staged\n");
        side.git(&["add", "b"]);
        for args in setup {
            let out = twin_repo::Side::git(side, args);
            assert_eq!(out.code, 0, "{args:?}: {out:?}");
        }
    }
    for (env, args) in cases {
        let want = s.git_env(env, args);
        assert!(want.stderr.contains("deprecated") || args.contains(&"-h"), "{args:?}: {want:?}");
        assert_eq!(z.git_env(env, args), want, "{env:?} {args:?}");
    }
}

#[test]
fn a_repository_file_setting_warns_and_advises() {
    compare(
        "autocc-local",
        &[&["config", "core.commentChar", "auto"]],
        &[(&[], &["commit", "-m", "x"])],
    );
    compare(
        "autocc-string",
        &[&["config", "core.commentString", "auto"]],
        &[(&[], &["commit", "--dry-run"])],
    );
}

#[test]
fn a_key_set_twice_in_one_file_is_unset_with_all() {
    compare(
        "autocc-twice",
        &[
            &["config", "core.commentChar", "auto"],
            &["config", "--add", "core.commentChar", "auto"],
        ],
        &[(&[], &["commit", "-m", "x"])],
    );
}

#[test]
fn every_file_is_listed_and_the_last_one_is_set() {
    compare(
        "autocc-both-keys",
        &[
            &["config", "core.commentString", "//"],
            &["config", "core.commentChar", "auto"],
        ],
        &[(&[], &["commit", "-m", "x"])],
    );
}

#[test]
fn a_command_line_value_warns_without_advice_unless_a_file_set_it_last() {
    compare(
        "autocc-cli",
        &[],
        &[(&[], &["-c", "core.commentChar=auto", "commit", "-m", "x"])],
    );
    compare(
        "autocc-cli-over-file",
        &[&["config", "core.commentChar", "auto"]],
        &[(&[], &["-c", "core.commentChar=auto", "commit", "-m", "x"])],
    );
}

#[test]
fn an_included_file_is_named_from_the_top_of_the_work_tree() {
    compare(
        "autocc-include",
        &[
            &["config", "-f", ".git/extra", "core.commentChar", "auto"],
            &["config", "include.path", "extra"],
        ],
        &[(&[], &["commit", "-m", "x"])],
    );
}

#[test]
fn the_other_four_builtins_warn_and_a_given_warning_is_not_repeated() {
    compare(
        "autocc-builtins",
        &[&["config", "core.commentChar", "auto"]],
        &[
            (&[], &["cherry-pick", "-h"]),
            (&[], &["revert", "-h"]),
            (&[], &["merge", "--no-edit", "side"]),
        ],
    );
    let Some(stock) = stock_git::stock_git_at_least((2, 52, 0)) else { return };
    let (s, z) = twin_repo::pair("autocc-given", stock);
    for side in [&s, &z] {
        side.git(&["config", "core.commentChar", "auto"]);
    }
    let env = [("GIT_AUTO_COMMENT_CHAR_CONFIG_WARNING_GIVEN", "1")];
    let args = ["commit", "--allow-empty", "-m", "x"];
    let want = s.git_env(&env, &args);
    assert_eq!(want.stderr, "");
    assert_eq!(z.git_env(&env, &args), want);
}
