//! `archive --remote` against a local repository, as `run_remote_archiver()`
//! (builtin/archive.c:23-71) opens it.
//!
//! * `remote_get()` resolves the operand: a configured remote's name stands for
//!   its URL, and `url.<base>.insteadOf` rewrites it. zvcs handed the raw name to
//!   `upload-archive`, so `--remote=origin` was "'origin' does not appear to be a
//!   git repository".
//! * `git_connect()` refuses a path that looks like an option ("strange pathname
//!   '-x' blocked", connect.c:1483-1484) and runs `--exec` as `<exec> '<path>'`
//!   through `/bin/sh -c`, whose `$0` is that whole command line.
//! * The serving side stats `--add-file` in the git directory `enter_repo()`
//!   chdir'd into, so `--add-file=HEAD` passes the stat and reaches "options
//!   '--add-file' and '--remote' cannot be used together" while `--add-file=a`
//!   (a work-tree file) is "File not found".
//! * When the server fails, its own `fatal: sent error to the client: ...` and
//!   the client's `remote: ` lines share one stderr. Their order is a race in
//!   stock git too, so those cases compare the lines as a set; each process
//!   writes each line in one `write()`, so no line may come out torn.
//!
//! Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin.rs"]
mod twin;

use twin::{Outcome, Twin};

/// `stderr` lines in sorted order: the two processes' lines interleave by
/// scheduling, in stock git as here.
fn lines(o: &Outcome) -> Vec<&str> {
    let mut v: Vec<&str> = o.stderr.lines().collect();
    v.sort_unstable();
    v
}

#[track_caller]
fn same_unordered(t: &Twin, args: &[&str]) {
    let (stock, zvcs) = t.run_in("work", args);
    assert_eq!(zvcs.code, stock.code, "git {args:?}: {zvcs:?} vs {stock:?}");
    assert_eq!(zvcs.stdout, stock.stdout, "git {args:?}");
    assert_eq!(lines(&zvcs), lines(&stock), "git {args:?}: left is zvcs, right is stock");
}

#[test]
fn a_remote_name_and_an_insteadof_url_resolve_as_remote_get_does() {
    let Some(t) = Twin::new("archive-remote-get") else { return };
    t.same(&["archive", "--remote=origin", "--list"]);
    t.same(&["archive", "--remote=origin", "--format=tar", "main"]);
    t.same(&["-c", "url.../.insteadOf=zz:", "archive", "--remote=zz:up", "--list"]);
    same_unordered(&t, &["archive", "--remote=origin", "nosuchref"]);
}

#[test]
fn git_connect_blocks_an_option_like_path_and_runs_exec_through_the_shell() {
    let Some(t) = Twin::new("archive-remote-connect") else { return };
    t.same(&["archive", "--remote=-x", "HEAD"]);
    t.same(&["archive", "--remote=file://-x", "HEAD"]);
    t.same(&["archive", "--remote=.", "--exec=nosuchcmd", "HEAD"]);
    t.same(&["archive", "--remote=../up", "--exec=nosuchcmd;", "--list"]);
}

#[test]
fn the_server_stats_add_file_in_the_git_directory() {
    let Some(t) = Twin::new("archive-remote-add-file") else { return };
    for args in [
        &["archive", "--remote=.", "--add-file=HEAD", "HEAD"][..],
        &["archive", "--remote=.", "--add-file=objects", "HEAD"],
        &["archive", "--remote=.", "--add-file=.git/HEAD", "HEAD"],
        &["archive", "--remote=.", "--add-file=nosuch", "HEAD"],
        &["archive", "--remote=../up", "--add-file=HEAD", "HEAD"],
    ] {
        same_unordered(&t, args);
    }
}

#[test]
fn a_failing_server_and_the_client_never_tear_each_others_lines() {
    let Some(t) = Twin::new("archive-remote-tear") else { return };
    let (stock, _) = t.run_in("work", &["archive", "--remote=nosuchdir", "HEAD"]);
    assert_eq!(stock.code, 1, "{stock:?}");
    for _ in 0..12 {
        let (_, zvcs) = t.run_in("work", &["archive", "--remote=nosuchdir", "HEAD"]);
        assert_eq!(zvcs.code, stock.code, "{zvcs:?}");
        assert_eq!(lines(&zvcs), lines(&stock), "left is zvcs, right is stock");
    }
}
