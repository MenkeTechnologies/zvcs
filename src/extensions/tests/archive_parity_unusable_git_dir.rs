//! `archive` and `grep` look for their repository only after their options are parsed.
//!
//! Both are `RUN_SETUP_GENTLY` in git.c, so a `--git-dir` that names no repository does not end
//! the command at setup: `archive --list` still lists the formats, a missing tree-ish or an
//! unknown option is the 129 usage error, `grep -h` prints its usage, and only a command that
//! really needs the repository dies with `not a git repository: '<dir>'`. zvcs refused every
//! invocation at setup with that last line and exit 128.
//!
//! Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin.rs"]
mod twin;

use twin::Twin;

#[test]
fn archive_parses_its_options_before_it_needs_the_repository() {
    let Some(t) = Twin::new("archive-gitdir") else { return };
    for args in [
        &["--git-dir=no-such", "archive", "--list"][..],
        &["--git-dir=no-such", "archive"],
        &["--git-dir=no-such", "archive", "-9"],
        &["--git-dir=no-such", "archive", "--prefix=a/b/", "--prefix=pfx/", "-9"],
        &["--git-dir=no-such", "archive", "--bogus-option", "HEAD"],
        &["--git-dir=no-such", "archive", "--format=bogus", "HEAD"],
        &["--git-dir=no-such", "archive", "HEAD"],
    ] {
        t.same(args);
    }
    let (stock, zvcs) = t.run_in("work", &["--git-dir=no-such", "archive", "--prefix=a/", "-9"]);
    assert_eq!(stock.code, 129, "{stock:?}");
    assert_eq!(zvcs, stock);
}

#[test]
fn grep_prints_its_usage_before_it_needs_the_repository() {
    let Some(t) = Twin::new("grep-gitdir") else { return };
    t.same(&["--git-dir=no-such", "grep", "-h"]);
    t.same(&["--git-dir=no-such", "grep", "x"]);
}

#[test]
fn a_command_that_sets_up_strictly_still_names_the_directory() {
    let Some(t) = Twin::new("strict-gitdir") else { return };
    let (stock, zvcs) = t.run_in("work", &["--git-dir=no-such", "status"]);
    assert_eq!(stock.code, 128, "{stock:?}");
    assert!(stock.stderr.contains("not a git repository: 'no-such'"), "{stock:?}");
    assert_eq!(zvcs, stock);
}
