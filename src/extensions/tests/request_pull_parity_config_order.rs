//! Where `git request-pull` meets configuration it refuses.
//!
//! The script sources `git-sh-setup`, which runs `git rev-parse` before the usage
//! test, so a refused `core.*` value (`git_default_config()`) ends the run at 128
//! ahead of the usage text, a missing revision and everything else, and outside a
//! repository the same call dies with `not a git repository`. The first command
//! that reads the diff configuration is `git show -s`, after the remote check
//! printed its `warn:` lines: its `fatal:` breaks the `&&` chain, nothing reaches
//! stdout, and the script exits 1. zvcs ran the whole report regardless.
//!
//! The remote check itself (`git ls-remote <url>`) runs from the top of the work
//! tree, so a URL of `.` typed from a subdirectory is the repository.
//!
//! Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin.rs"]
mod twin;

use twin::Twin;

#[test]
fn a_refused_core_value_precedes_everything() {
    let Some(t) = Twin::new("rp-core") else { return };
    for cfg in ["core.abbrev=true", "core.abbrev=1", "core.ignorecase=auto", "core.quotePath=maybe"] {
        t.same(&["-c", cfg, "request-pull", "v0.1.0", "../up", "HEAD"]);
        // Even the usage text and an unknown revision wait behind it.
        t.same(&["-c", cfg, "request-pull", "-p"]);
        t.same(&["-c", cfg, "request-pull", "no-such-rev", "../up"]);
    }
}

#[test]
fn outside_a_repository_it_is_the_setup_failure() {
    let Some(t) = Twin::new("rp-outside") else { return };
    t.same_in(".", &["request-pull"]);
    t.same_in(".", &["request-pull", "v0.1.0", "up"]);
}

#[test]
fn a_refused_diff_value_stops_the_report_before_stdout() {
    let Some(t) = Twin::new("rp-diff") else { return };
    for cfg in ["diff.context=true", "diff.statNameWidth=warn", "diff.renameLimit=x"] {
        t.same(&["-c", cfg, "request-pull", "v0.1.0", "../up", "HEAD"]);
        t.same(&["-c", cfg, "request-pull", "-p", "v0.1.0", "../up", "HEAD"]);
    }
}

#[test]
fn a_valid_config_still_reports() {
    let Some(t) = Twin::new("rp-valid") else { return };
    t.same(&["-c", "diff.context=5", "request-pull", "-p", "v0.1.0", "../up", "HEAD"]);
}

#[test]
fn dot_from_a_subdirectory_is_the_work_tree_top() {
    let Some(t) = Twin::new("rp-subdir") else { return };
    t.mkdir("work/src");
    t.same_in("work/src", &["request-pull", "v0.1.0", ".", "HEAD"]);
}
