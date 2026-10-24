//! Every status format that reports the branch calls `branch_get()`, and
//! remote.c's `read_config()` behind it parses every `remote.*` / `branch.*`
//! key, so a bad `remote.<name>.prune` dies there with whatever the printer had
//! already written still in stdio's buffer, which `die()`'s `exit()` flushes:
//!
//!   * `wt_shortstatus_print_tracking()` (wt-status.c:2206-2226) after `## `
//!     (and `No commits yet on ` for an unborn branch); a detached `HEAD` jumps
//!     to `conclude` before the call and prints in full;
//!   * `wt_longstatus_print_tracking()` (wt-status.c:1226-1229), reached only for
//!     a born `refs/heads/` branch (wt-status.c:2025), after `On branch <name>`.
//!     `git commit`'s editor block runs the same printer into `COMMIT_EDITMSG`
//!     (builtin/commit.c:911), so the file keeps the header line.
//!
//! The port computed the whole report first and printed it without ever reading
//! remote config. Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin.rs"]
mod twin;

use twin::Twin;

const BAD: [&str; 2] = ["-c", "remote.origin.prune=="];

fn with_bad(args: &[&'static str]) -> Vec<&'static str> {
    BAD.iter().chain(args).copied().collect()
}

#[test]
fn short_and_long_formats_die_after_their_header() {
    let Some(t) = Twin::new("status-branch-get") else { return };
    for args in [
        &["status"][..],
        &["status", "-sb"],
        &["status", "-s", "-b", "-z"],
        &["status", "--porcelain", "-b"],
        &["-c", "status.branch=true", "status", "-s"],
        &["-c", "status.displayCommentPrefix=true", "status"],
        &["-c", "color.status=always", "-c", "color.status.header=blue", "status", "-sb"],
        &["-c", "color.status=always", "-c", "color.status.branch=red", "status"],
        // The die precedes `format_tracking_info()`, so its warning never prints.
        &["-c", "status.compareBranches=bogus", "status"],
        &["commit", "--dry-run"],
        // No `-b`: neither printer is asked for the branch, nothing dies.
        &["status", "-s"],
        &["status", "--porcelain"],
    ] {
        let args = with_bad(args);
        let (stock, zvcs) = t.run_in("work", &args);
        assert_eq!(zvcs, stock, "git {args:?}: left is zvcs, right is stock");
    }
    let (stock, _) = t.run_in("work", &with_bad(&["status", "-sb"]));
    assert_eq!((stock.code, stock.stdout.as_str()), (128, "## "));
}

#[test]
fn detached_and_unborn_heads_reach_the_call_or_not_as_stock_does() {
    let Some(t) = Twin::new("status-branch-get-head") else { return };
    t.prepare(&["checkout", "-q", "--detach"]);
    for args in [&["status"][..], &["status", "-sb"]] {
        let args = with_bad(args);
        let (stock, zvcs) = t.run_in("work", &args);
        assert_eq!(zvcs, stock, "detached: git {args:?}");
    }
    t.prepare(&["checkout", "-q", "--orphan", "fresh"]);
    for args in [&["status"][..], &["status", "-sb"], &["status", "-sbz"]] {
        let args = with_bad(args);
        let (stock, zvcs) = t.run_in("work", &args);
        assert_eq!(zvcs, stock, "unborn: git {args:?}");
    }
}

#[test]
fn the_editor_block_keeps_the_header_it_wrote_before_dying() {
    let Some(t) = Twin::new("status-branch-get-editmsg") else { return };
    let args = with_bad(&["commit", "--allow-empty"]);
    let (stock, zvcs) = t.run_env_in("work", &args, &[("GIT_EDITOR", "true")]);
    assert_eq!(zvcs, stock);
    assert_eq!(stock.code, 128);
    let (stock_msg, zvcs_msg) = t.read("work/.git/COMMIT_EDITMSG");
    assert!(stock_msg.as_deref().is_some_and(|m| m.ends_with("# On branch main\n")), "{stock_msg:?}");
    assert_eq!(zvcs_msg, stock_msg);
}
