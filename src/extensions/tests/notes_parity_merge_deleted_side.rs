//! `git notes merge` where one side deleted the note the other side changed, a
//! three-way content conflict, `merge --commit` onto a notes ref that moved, and an
//! empty note in `log`.
//!
//! * `merge_one_change_manual()` (notes-merge.c:408-439) reports a deleted side as
//!   `CONFLICT (delete/modify)` and leaves the surviving note in the worktree as it
//!   is; zvcs called it a content conflict and wrote markers around an empty side.
//! * `ll_merge_in_worktree()` (notes-merge.c:355-384) runs `ll_merge()` with the
//!   merge base, so only the conflicting hunk carries markers; zvcs wrapped both
//!   whole notes.
//! * `-s union` / `-s cat_sort_uniq` `add_note()` the remote note into the local
//!   tree (notes-merge.c:466-484), where a null side adds nothing
//!   (`combine_notes_concatenate()`, notes.c:810-856, `note_tree_insert()`,
//!   notes.c:253-286); zvcs concatenated an empty blob onto the survivor.
//! * `merge --commit` updates the ref with the partial commit's first parent as its
//!   old value (builtin/notes.c:875-878) and dies when the ref moved.
//! * `format_note()` (notes.c:1328) prints no line for an empty note.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

/// Three commits and four notes in `refs/notes/base`. Against it
/// `refs/notes/commits` edits the first line of `c1`'s note, rewrites `c2`'s and
/// `f2`'s and removes `f1`'s; `refs/notes/theirs` edits the last line of `c1`'s,
/// rewrites `c2`'s, `f1`'s and `c3`'s and removes `f2`'s. `refs/notes/emp`
/// holds an empty note.
const FIXTURE: &str = r#"
set -e
t=1700000000
c() { t=$((t+3600)); GIT_AUTHOR_DATE="@$t +0000" GIT_COMMITTER_DATE="@$t +0000" "$GIT" commit -q -m "$1"; }
N() { t=$((t+60)); GIT_AUTHOR_DATE="@$t +0000" GIT_COMMITTER_DATE="@$t +0000" "$GIT" notes "$@"; }
"$GIT" init -q -b main .
for i in 1 2 3; do echo $i > f$i; "$GIT" add f$i; c c$i; done
N add -m "one" -m "two" HEAD~2
N add -m "c2" HEAD~1
N add -m "blob" HEAD:f1
N add -m "f2" HEAD:f2
N add -m "c3" HEAD
"$GIT" update-ref refs/notes/base refs/notes/commits
N add -f -m "ONE" -m "two" HEAD~2
N add -f -m "c2 local" HEAD~1
N remove HEAD:f1 2>/dev/null
N add -f -m "f2 local" HEAD:f2
"$GIT" update-ref refs/notes/local refs/notes/commits
"$GIT" update-ref refs/notes/commits refs/notes/base
N add -f -m "one" -m "TWO" HEAD~2
N add -f -m "c2 theirs" HEAD~1
N add -f -m "blob theirs" HEAD:f1
N add -f -m "c3 theirs" HEAD
N remove HEAD:f2 2>/dev/null
"$GIT" update-ref refs/notes/theirs refs/notes/commits
"$GIT" update-ref refs/notes/commits refs/notes/local
"$GIT" update-ref -d refs/notes/local
N --ref=emp add --allow-empty HEAD
"#;

struct Twin {
    root: PathBuf,
}

impl Drop for Twin {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Twin {
    fn new(stock: &str, label: &str) -> Twin {
        let root = std::env::temp_dir().join(format!("zvcs-notes-merge-del-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for side in ["stock", "zvcs"] {
            let dir = root.join(side);
            std::fs::create_dir_all(&dir).unwrap();
            let out = env(&mut Command::new("sh"), &root)
                .args(["-c", FIXTURE])
                .env("GIT", stock)
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(out.status.success(), "fixture: {}", String::from_utf8_lossy(&out.stderr));
        }
        Twin { root }
    }

    /// Run `script` under `sh` with `$GIT` the binary for `side`.
    fn sh(&self, bin: &str, side: &str, script: &str) -> (String, String, i32) {
        let out = env(&mut Command::new("sh"), &self.root)
            .args(["-c", script])
            .env("GIT", bin)
            .env("STOCK", stock_git().unwrap())
            .current_dir(self.root.join(side))
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().unwrap_or(-1),
        )
    }

    /// The script's output and the state it left, read back with stock git.
    fn same(&self, stock: &str, script: &str) {
        let state = r#"; echo "rc=$?"; "$STOCK" for-each-ref; for r in $("$STOCK" for-each-ref --format='%(refname)' refs/notes); do echo "== $r"; "$STOCK" notes --ref=$r list; done; for f in .git/NOTES_MERGE_WORKTREE/*; do [ -f "$f" ] && { echo "-- $f"; cat "$f"; }; done; cat .git/NOTES_MERGE_REF 2>/dev/null; true"#;
        let script = format!("{script}{state}");
        assert_eq!(self.sh(ZVCS, "zvcs", &script), self.sh(stock, "stock", &script), "{script}");
    }
}

fn env<'a>(cmd: &'a mut Command, root: &Path) -> &'a mut Command {
    cmd.env("HOME", root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1800000000 +0000")
        .env("GIT_COMMITTER_DATE", "1800000000 +0000")
        .env("ZVCS_HOME", root.join("zvcs-home"))
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_NOTES_REF")
}

#[test]
fn manual_merge_reports_delete_modify_and_merges_content_three_way() {
    let Some(stock) = stock_git() else { return };
    let t = Twin::new(stock, "manual");
    t.same(stock, r#""$GIT" notes merge theirs"#);
    t.same(stock, r#""$GIT" notes merge -v theirs"#);
    t.same(stock, r#""$GIT" notes merge -q theirs"#);
    t.same(stock, r#""$GIT" notes --ref=theirs merge commits"#);
}

#[test]
fn union_and_cat_sort_uniq_skip_a_deleted_side() {
    let Some(stock) = stock_git() else { return };
    let t = Twin::new(stock, "union");
    t.same(stock, r#""$GIT" notes merge -s union theirs; "$STOCK" notes show HEAD~2"#);
    t.same(stock, r#""$GIT" notes merge -s cat_sort_uniq theirs; "$STOCK" notes show HEAD~2"#);
    t.same(stock, r#""$GIT" -c notes.mergeStrategy=union notes --ref=theirs merge commits"#);
}

#[test]
fn merge_commit_dies_when_the_notes_ref_moved() {
    let Some(stock) = stock_git() else { return };
    let t = Twin::new(stock, "moved");
    t.same(stock, r#""$GIT" notes merge theirs >/dev/null; "$STOCK" update-ref refs/notes/commits refs/notes/base; "$GIT" notes merge --commit"#);
    t.same(stock, r#""$GIT" notes merge theirs >/dev/null; "$STOCK" update-ref -d refs/notes/commits; "$GIT" notes merge --commit"#);
    t.same(stock, r#""$GIT" notes merge theirs >/dev/null; "$GIT" notes merge --commit"#);
}

#[test]
fn an_empty_note_prints_no_line() {
    let Some(stock) = stock_git() else { return };
    let t = Twin::new(stock, "empty");
    t.same(stock, r#""$GIT" log -1 --notes=emp --format=medium"#);
    t.same(stock, r#""$GIT" show -s --notes=emp HEAD"#);
    t.same(stock, r#""$GIT" log -1 --notes=emp --format="[%N]""#);
}
