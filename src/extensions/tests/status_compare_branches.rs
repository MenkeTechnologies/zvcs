//! `status.compareBranches` — the branch comparisons `git status`'s long format
//! prints, and the three advice flags each one carries.
//!
//! git 2.55.0 reads the key inside `format_tracking_info()` (`remote.c:2375-2465`),
//! which is the whole of the long format's tracking block and is also what
//! `builtin/checkout.c:941` prints after a branch switch:
//!
//! ```c
//! repo_config_get_string(the_repository, "status.comparebranches",
//!                        &compare_branches);
//!
//! if (compare_branches) {
//!         string_list_split(&branches, compare_branches, " ", -1);
//!         string_list_remove_empty_items(&branches, 0);
//! } else {
//!         string_list_append(&branches, "@{upstream}");
//! }
//! ```
//!
//! Each surviving name goes through `resolve_compare_branch()`
//! (`remote.c:2291-2312`), which accepts `@{upstream}` and `@{push}`
//! case-insensitively and warns about everything else, and each resolved ref is
//! compared once — `strset_add(&processed_refs, full_ref)` (`remote.c:2412`)
//! drops a repeat.
//!
//! The part that changes output even with the key *unset* is the flag block at
//! `remote.c:2420-2452`:
//!
//! ```c
//! is_upstream = upstream_ref && !strcmp(full_ref, upstream_ref);
//! is_push = push_ref && !strcmp(full_ref, push_ref);
//!
//! if (is_upstream && (!push_ref || !strcmp(upstream_ref, push_ref)))
//!         is_push = 1;
//! ```
//!
//! `format_branch_comparison()` (`remote.c:2314-2370`) gates the
//! `(use "git push" …)` hint on `is_push`, so a branch whose `@{push}` differs
//! from its `@{upstream}` — the effect of `remote.pushDefault` — loses that hint
//! on the default, upstream-only comparison. Every byte below was captured from
//! stock git 2.55.0 against the same fixture this file builds.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A scratch directory of our own, since this crate carries no `tempfile`
/// dev-dependency.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-status-compare-branches-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// The PATH with this repository's shim removed, so a nested call cannot reach
/// the installed `zvcs` instead of the binary under test.
fn real_git_path() -> String {
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|p| !p.contains(".zvcs"))
        .collect::<Vec<_>>()
        .join(":")
}

fn git(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .env("PATH", real_git_path())
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .current_dir(dir)
        .output()
        .expect("run the binary under test")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// `main` two commits ahead of `origin/main` and one ahead of `fork/main`, with
/// `remote.pushDefault=fork` so `@{upstream}` and `@{push}` name different refs.
///
/// The remote-tracking refs are written directly rather than fetched: nothing
/// here needs a transport, and a hand-built ref set makes the counts exact.
fn fixture(name: &str) -> PathBuf {
    let dir = scratch(name);
    git(&dir, &["init", "-q", "-b", "main", "."]);
    git(&dir, &["commit", "-q", "--allow-empty", "-m", "c1"]);
    let c1 = stdout(&git(&dir, &["rev-parse", "HEAD"])).trim().to_string();
    git(&dir, &["commit", "-q", "--allow-empty", "-m", "c2"]);
    let c2 = stdout(&git(&dir, &["rev-parse", "HEAD"])).trim().to_string();
    git(&dir, &["commit", "-q", "--allow-empty", "-m", "c3"]);

    git(&dir, &["update-ref", "refs/remotes/origin/main", &c1]);
    git(&dir, &["update-ref", "refs/remotes/fork/main", &c2]);
    for (key, value) in [
        ("remote.origin.url", "."),
        ("remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"),
        ("remote.fork.url", "."),
        ("remote.fork.fetch", "+refs/heads/*:refs/remotes/fork/*"),
        ("branch.main.remote", "origin"),
        ("branch.main.merge", "refs/heads/main"),
        ("remote.pushDefault", "fork"),
        ("push.default", "current"),
    ] {
        git(&dir, &["config", key, value]);
    }
    dir
}

/// `git status` with the key set to `value`, or unset when `value` is `None`.
fn status(dir: &Path, value: Option<&str>) -> Output {
    match value {
        Some(v) => git(dir, &["-c", &format!("status.compareBranches={v}"), "status"]),
        None => git(dir, &["status"]),
    }
}

/// The unset default is one comparison against `@{upstream}` — and, because
/// `remote.pushDefault` gives this branch a `@{push}` of its own, that
/// comparison is *not* the push destination, so `remote.c:2449` never sets
/// `ENABLE_ADVICE_PUSH` and the `(use "git push" …)` line is absent.
///
/// This is the one assertion here that fails without reading the key at all: the
/// pre-`status.compareBranches` shape of `format_tracking_info` had no notion of
/// a push destination distinct from the upstream and printed the hint anyway.
#[test]
fn the_default_compares_the_upstream_and_withholds_the_push_hint() {
    let dir = fixture("default");
    let out = status(&dir, None);
    assert_eq!(
        stdout(&out),
        "On branch main\n\
         Your branch is ahead of 'origin/main' by 2 commits.\n\
         \n\
         nothing to commit, working tree clean\n"
    );
    assert_eq!(stderr(&out), "");
}

/// Dropping `remote.pushDefault` makes `@{push}` resolve to the upstream again,
/// which restores the hint through the second half of `remote.c:2423-2424`.
/// Same repository, same comparison, one config key apart.
#[test]
fn the_push_hint_returns_when_the_push_destination_is_the_upstream() {
    let dir = fixture("same-push");
    git(&dir, &["config", "--unset", "remote.pushDefault"]);
    assert_eq!(
        stdout(&status(&dir, None)),
        "On branch main\n\
         Your branch is ahead of 'origin/main' by 2 commits.\n\
         \x20 (use \"git push\" to publish your local commits)\n\
         \n\
         nothing to commit, working tree clean\n"
    );
}

/// Two names, two comparisons, in configuration order, separated by the blank
/// line `remote.c:2444-2445` writes before every entry after the first. Only the
/// `@{push}` one carries the push hint.
#[test]
fn two_names_produce_two_comparisons_in_order() {
    let dir = fixture("two");
    assert_eq!(
        stdout(&status(&dir, Some("@{upstream} @{push}"))),
        "On branch main\n\
         Your branch is ahead of 'origin/main' by 2 commits.\n\
         \n\
         Your branch is ahead of 'fork/main' by 1 commit.\n\
         \x20 (use \"git push\" to publish your local commits)\n\
         \n\
         nothing to commit, working tree clean\n"
    );
}

/// `@{push}` alone drops the upstream comparison entirely, and the surviving one
/// is the push destination — proof the names are resolved rather than merely
/// counted.
#[test]
fn the_push_destination_can_be_the_only_comparison() {
    let dir = fixture("push-only");
    assert_eq!(
        stdout(&status(&dir, Some("@{push}"))),
        "On branch main\n\
         Your branch is ahead of 'fork/main' by 1 commit.\n\
         \x20 (use \"git push\" to publish your local commits)\n\
         \n\
         nothing to commit, working tree clean\n"
    );
}

/// `strset_add()` (`remote.c:2412`) suppresses the repeat, so the three names
/// here yield two comparisons — and the first occurrence is the one kept, which
/// is why `fork/main` leads.
#[test]
fn a_repeated_name_is_compared_once() {
    let dir = fixture("dedup");
    assert_eq!(
        stdout(&status(&dir, Some("@{push} @{upstream} @{push}"))),
        "On branch main\n\
         Your branch is ahead of 'fork/main' by 1 commit.\n\
         \x20 (use \"git push\" to publish your local commits)\n\
         \n\
         Your branch is ahead of 'origin/main' by 2 commits.\n\
         \n\
         nothing to commit, working tree clean\n"
    );
}

/// `resolve_compare_branch()`'s `else` arm: one warning per occurrence, on
/// stderr, and the name contributes no comparison. The command still succeeds.
#[test]
fn an_unsupported_name_warns_once_per_occurrence_and_is_skipped() {
    let dir = fixture("warn");

    let out = status(&dir, Some("zzz @{upstream}"));
    assert_eq!(
        stderr(&out),
        "warning: ignoring value 'zzz' for status.compareBranches, \
         only @{upstream} and @{push} are supported\n"
    );
    assert_eq!(
        stdout(&out),
        "On branch main\n\
         Your branch is ahead of 'origin/main' by 2 commits.\n\
         \n\
         nothing to commit, working tree clean\n"
    );
    assert_eq!(out.status.code(), Some(0));

    let two = status(&dir, Some("zzz yyy"));
    assert_eq!(
        stderr(&two),
        "warning: ignoring value 'zzz' for status.compareBranches, \
         only @{upstream} and @{push} are supported\n\
         warning: ignoring value 'yyy' for status.compareBranches, \
         only @{upstream} and @{push} are supported\n"
    );
    // Every name was rejected, so `reported` stays 0 and the tracking block —
    // including the blank line `wt_longstatus_print_tracking` writes under it —
    // is absent altogether.
    assert_eq!(
        stdout(&two),
        "On branch main\nnothing to commit, working tree clean\n"
    );
}

/// `strcasecmp()`, not `strcmp()` (`remote.c:2298`).
#[test]
fn the_two_names_are_matched_case_insensitively() {
    let dir = fixture("case");
    assert_eq!(
        stdout(&status(&dir, Some("@{UPSTREAM}"))),
        stdout(&status(&dir, Some("@{upstream}"))),
    );
    assert_eq!(
        stdout(&status(&dir, Some("@{Push}"))),
        stdout(&status(&dir, Some("@{push}"))),
    );
}

/// The delimiter is one literal space. A tab is part of the name, so this is a
/// single unrecognised entry and the warning quotes it whole — the tab included.
#[test]
fn a_tab_is_not_a_separator() {
    let dir = fixture("tab");
    let out = status(&dir, Some("@{upstream}\t@{push}"));
    assert_eq!(
        stderr(&out),
        "warning: ignoring value '@{upstream}\t@{push}' for status.compareBranches, \
         only @{upstream} and @{push} are supported\n"
    );
    assert_eq!(
        stdout(&out),
        "On branch main\nnothing to commit, working tree clean\n"
    );
}

/// `string_list_remove_empty_items()` leaves nothing to compare, so an empty
/// value is not "the default" — it is no tracking block at all, and no warning.
#[test]
fn an_empty_value_removes_the_tracking_block() {
    let dir = fixture("empty");
    let out = status(&dir, Some(""));
    assert_eq!(
        stdout(&out),
        "On branch main\nnothing to commit, working tree clean\n"
    );
    assert_eq!(stderr(&out), "");
    // Runs of spaces collapse the same way, for the same reason.
    assert_eq!(stdout(&status(&dir, Some("   "))), stdout(&out));
}

/// `cmp < 0` (`remote.c:2429-2442`): the "upstream is gone" sentence belongs to
/// the upstream entry alone. With the upstream ref deleted, `@{push}` still
/// compares normally and the upstream entry reports the loss — and the gone
/// report takes no preceding blank line, because the separator at
/// `remote.c:2444` is only reached by entries that got as far as a comparison.
#[test]
fn a_missing_base_ref_is_reported_only_for_the_upstream() {
    let dir = fixture("gone");
    git(&dir, &["update-ref", "-d", "refs/remotes/origin/main"]);

    assert_eq!(
        stdout(&status(&dir, None)),
        "On branch main\n\
         Your branch is based on 'origin/main', but the upstream is gone.\n\
         \x20 (use \"git branch --unset-upstream\" to fixup)\n\
         \n\
         nothing to commit, working tree clean\n"
    );

    // The push comparison alone says nothing about the missing upstream.
    assert_eq!(
        stdout(&status(&dir, Some("@{push}"))),
        "On branch main\n\
         Your branch is ahead of 'fork/main' by 1 commit.\n\
         \x20 (use \"git push\" to publish your local commits)\n\
         \n\
         nothing to commit, working tree clean\n"
    );

    // Both, in that order: the gone report first, then the push comparison with
    // the blank line the second entry brings.
    assert_eq!(
        stdout(&status(&dir, Some("@{upstream} @{push}"))),
        "On branch main\n\
         Your branch is based on 'origin/main', but the upstream is gone.\n\
         \x20 (use \"git branch --unset-upstream\" to fixup)\n\
         \n\
         Your branch is ahead of 'fork/main' by 1 commit.\n\
         \x20 (use \"git push\" to publish your local commits)\n\
         \n\
         nothing to commit, working tree clean\n"
    );
}

/// A branch that tracks nothing resolves neither name, so the key adds no
/// comparisons — but an unsupported name still warns, because
/// `resolve_compare_branch()` warns before it resolves anything.
#[test]
fn a_branch_with_no_upstream_gets_no_comparisons() {
    let dir = fixture("solo");
    git(&dir, &["checkout", "-q", "-b", "solo"]);

    assert_eq!(
        stdout(&status(&dir, Some("@{upstream} @{push}"))),
        "On branch solo\nnothing to commit, working tree clean\n"
    );
    assert_eq!(
        stderr(&status(&dir, Some("nope"))),
        "warning: ignoring value 'nope' for status.compareBranches, \
         only @{upstream} and @{push} are supported\n"
    );
}

/// `wt_shortstatus_print_tracking()` and the porcelain formats call
/// `stat_tracking_info()` directly and never reach `format_tracking_info()`, so
/// the key must not touch them however many names it lists.
#[test]
fn the_short_and_porcelain_formats_ignore_the_key() {
    let dir = fixture("short");
    let names = "@{push} @{upstream}";

    let short = |v: Option<&str>| match v {
        Some(v) => git(
            &dir,
            &["-c", &format!("status.compareBranches={v}"), "status", "-sb"],
        ),
        None => git(&dir, &["status", "-sb"]),
    };
    assert_eq!(stdout(&short(None)), "## main...origin/main [ahead 2]\n");
    assert_eq!(stdout(&short(Some(names))), stdout(&short(None)));

    let v2 = |v: Option<&str>| match v {
        Some(v) => git(
            &dir,
            &[
                "-c",
                &format!("status.compareBranches={v}"),
                "status",
                "--porcelain=v2",
                "--branch",
            ],
        ),
        None => git(&dir, &["status", "--porcelain=v2", "--branch"]),
    };
    assert!(
        stdout(&v2(None)).contains("# branch.ab +2 -0\n"),
        "porcelain v2 reports the upstream counts: {}",
        stdout(&v2(None))
    );
    assert_eq!(stdout(&v2(Some(names))), stdout(&v2(None)));
}

/// `builtin/checkout.c:941` prints the same block after a branch switch, so the
/// key reaches it too — and so does the flag that withholds the push hint.
#[test]
fn a_branch_switch_prints_the_same_comparisons() {
    let dir = fixture("checkout");
    git(&dir, &["checkout", "-q", "-b", "detour"]);

    let out = git(&dir, &["checkout", "main"]);
    assert_eq!(
        stdout(&out),
        "Your branch is ahead of 'origin/main' by 2 commits.\n"
    );
    assert_eq!(stderr(&out), "Switched to branch 'main'\n");

    git(&dir, &["checkout", "-q", "detour"]);
    let both = git(
        &dir,
        &[
            "-c",
            "status.compareBranches=@{upstream} @{push}",
            "checkout",
            "main",
        ],
    );
    assert_eq!(
        stdout(&both),
        "Your branch is ahead of 'origin/main' by 2 commits.\n\
         \n\
         Your branch is ahead of 'fork/main' by 1 commit.\n\
         \x20 (use \"git push\" to publish your local commits)\n"
    );
}
