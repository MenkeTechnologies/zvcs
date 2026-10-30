//! `git rebase -r` stopped on a conflicted `merge` against stock git: `wt_longstatus_print_state()`
//! prints `show_rebase_information()`'s block, then a raw `fputs("\n", s->fp)`, then the merge
//! banner (wt-status.c:1971-1976). That newline bypasses `status_printf`, so it carries no comment
//! prefix in the `COMMIT_EDITMSG` template `rebase --continue` and `commit` write, nor under
//! `status.displayCommentPrefix`.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use twin_repo::Side;

/// `topic` merges `fside` over a conflict on `f` resolved as `st`; `main` gains `h`, so
/// `rebase -r main` redoes the merge, stops on the same conflict, and `f` is resolved again.
fn stopped_merge(side: &Side) {
    side.write("f", "base\n");
    side.git(&["add", "f"]);
    side.git(&["commit", "-q", "-m", "f"]);
    side.git(&["checkout", "-q", "-b", "fside"]);
    side.write("f", "s\n");
    side.git(&["commit", "-q", "-am", "fside"]);
    side.git(&["checkout", "-q", "-b", "topic", "main"]);
    side.write("f", "t\n");
    side.git(&["commit", "-q", "-am", "t1"]);
    side.git(&["merge", "-q", "-m", "Merge fside", "fside"]);
    side.write("f", "st\n");
    side.git(&["add", "f"]);
    side.git(&["commit", "-q", "--no-edit"]);
    side.git(&["checkout", "-q", "main"]);
    side.write("h", "h\n");
    side.git(&["add", "h"]);
    side.git(&["commit", "-q", "-m", "mainh"]);
    side.git(&["checkout", "-q", "topic"]);
    let stop = side.git(&["rebase", "-r", "main"]);
    assert_eq!(stop.code, 1, "{stop:?}");
    side.write("f", "st\n");
    side.git(&["add", "f"]);
}

fn both(label: &str) -> Option<(Side, Side)> {
    let stock = stock_git::stock_git()?;
    let (s, z) = twin_repo::pair(label, stock);
    stopped_merge(&s);
    stopped_merge(&z);
    Some((s, z))
}

#[test]
fn the_template_has_a_bare_blank_line_between_the_rebase_and_merge_blocks() {
    for (label, args) in [
        ("rebase-merge-stop-continue", &["rebase", "--continue"][..]),
        ("rebase-merge-stop-commit", &["commit"]),
        ("rebase-merge-stop-commit-v", &["commit", "-v"]),
    ] {
        let Some((s, z)) = both(label) else {
            return;
        };
        assert_eq!(z.git(args), s.git(args), "{args:?}");
        assert_eq!(z.read(".git/COMMIT_EDITMSG"), s.read(".git/COMMIT_EDITMSG"), "COMMIT_EDITMSG after {args:?}");
        assert_eq!(z.git(&["log", "--format=%H %P%n%B", "-1"]), s.git(&["log", "--format=%H %P%n%B", "-1"]));
    }
}

#[test]
fn display_comment_prefix_leaves_that_line_unprefixed() {
    let Some((s, z)) = both("rebase-merge-stop-status") else {
        return;
    };
    for args in [
        &["status"][..],
        &["-c", "status.displayCommentPrefix=true", "status"],
        &["-c", "status.displayCommentPrefix=true", "-c", "status.submoduleSummary=true", "status"],
        &["-c", "status.displayCommentPrefix=true", "-c", "color.status=always", "status"],
        &["-c", "core.commentChar=;", "commit", "--dry-run"],
    ] {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
    }
}
