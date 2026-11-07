//! `git notes merge` conflicts go through `ll_merge()`, and `cat_sort_uniq`
//! fails over a note it cannot read.
//!
//! * `ll_merge_in_worktree()` (notes-merge.c:355-384) calls `ll_merge()` with the
//!   object's hex name as the path, so the `merge` and `conflict-marker-size`
//!   attributes of that name pick the driver and the marker width
//!   (merge-ll.c:409-451): `merge=union`, `-merge`, `merge.default`, and a
//!   `merge.<name>.driver` command with its `%O %A %B %L %P %S %X %Y`
//!   expansion, run from the top of the work tree; zvcs always ran the built-in
//!   text merge with 7-wide markers.
//! * A binary note is `ll_binary_merge()`'s conflict: the local note is left in
//!   the worktree and `Cannot merge binary files` is warned (notes-merge.c:376-378).
//! * A driver killed by a signal is `LL_MERGE_ERROR`, which dies with `Failed to
//!   execute internal merge`; earlier conflicts stay written.
//! * `combine_notes_cat_sort_uniq()` (notes.c:912-939) fails over a note blob it
//!   cannot read, so `-s cat_sort_uniq` dies and `copy --for-rewrite` reports the
//!   pair; a null source is still combined into the target (notes.c:1353-1368).
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

/// Two commits whose notes conflict: `base` in `refs/notes/base`, `local` on
/// `refs/notes/commits` and `remote` on `refs/notes/other`, for each commit. A
/// third commit carries a binary note that conflicts the same way on
/// `refs/notes/bin`/`refs/notes/binother`.
const FIXTURE: &str = r#"
set -e
t=1700000000
c() { t=$((t+3600)); GIT_AUTHOR_DATE="@$t +0000" GIT_COMMITTER_DATE="@$t +0000" "$GIT" commit -q --allow-empty -m "$1"; }
N() { t=$((t+60)); GIT_AUTHOR_DATE="@$t +0000" GIT_COMMITTER_DATE="@$t +0000" "$GIT" notes "$@"; }
"$GIT" init -q -b main .
c c1; c c2
for r in HEAD HEAD~1; do printf 'base\n' | N add -F - $r; done
"$GIT" update-ref refs/notes/other refs/notes/commits
for r in HEAD HEAD~1; do printf 'local\n' | N add -f -F - $r 2>/dev/null; done
for r in HEAD HEAD~1; do printf 'remote\n' | N --ref=other add -f -F - $r 2>/dev/null; done
printf 'a\0b\n' | N --ref=bin add -F - HEAD
"$GIT" update-ref refs/notes/binother refs/notes/bin
printf 'a\0c\n' | N --ref=bin add -f -F - HEAD 2>/dev/null
printf 'a\0d\n' | N --ref=binother add -f -F - HEAD 2>/dev/null
mkdir sub
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
    fn new(label: &str) -> Twin {
        let root = std::env::temp_dir().join(format!("zvcs-notes-llmerge-{label}-{}", std::process::id()));
        Twin { root }
    }

    /// Build the fixture afresh on both sides, so every case starts clean.
    fn reset(&self, stock: &str) {
        let _ = std::fs::remove_dir_all(&self.root);
        for side in ["stock", "zvcs"] {
            let dir = self.root.join(side);
            std::fs::create_dir_all(&dir).unwrap();
            let out = env(&mut Command::new("sh"), &self.root)
                .args(["-c", FIXTURE])
                .env("GIT", stock)
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(out.status.success(), "fixture: {}", String::from_utf8_lossy(&out.stderr));
        }
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

    /// The script's output and the state it left, read back with stock git: the
    /// refs, every worktree note byte for byte, and any temporary file a driver
    /// run left behind.
    fn same(&self, stock: &str, script: &str) {
        self.reset(stock);
        let state = r#"; echo "rc=$?"; cd "$TOP"; "$STOCK" for-each-ref; for f in .git/NOTES_MERGE_WORKTREE/*; do [ -f "$f" ] && { echo "-- $f"; od -c "$f"; }; done; ls -a; "$STOCK" status --porcelain; true"#;
        let script = format!("TOP=$PWD; {script}{state}");
        assert_eq!(self.sh(ZVCS, "zvcs", &script), self.sh(stock, "stock", &script), "{script}");
    }
}

fn env<'a>(cmd: &'a mut Command, root: &Path) -> &'a mut Command {
    cmd.env("HOME", root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "maintenance.auto")
        .env("GIT_CONFIG_VALUE_0", "false")
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
fn attributes_pick_the_builtin_driver_and_marker_size() {
    let Some(stock) = stock_git() else { return };
    let t = Twin::new("builtin");
    t.same(stock, r#"echo '* merge=union' > .gitattributes; "$GIT" notes merge -v other"#);
    t.same(stock, r#"echo '* -merge' > .git/info/attributes; "$GIT" notes merge other"#);
    t.same(stock, r#""$STOCK" config merge.default binary; "$GIT" notes merge other"#);
    t.same(stock, r#"echo '* conflict-marker-size=10' > .gitattributes; "$GIT" notes merge other"#);
    t.same(stock, r#"echo '* conflict-marker-size=x' > .gitattributes; "$GIT" notes merge other"#);
}

#[test]
fn a_binary_note_is_left_as_ours_with_a_warning() {
    let Some(stock) = stock_git() else { return };
    let t = Twin::new("binary");
    t.same(stock, r#""$GIT" notes --ref=bin merge binother"#);
    t.same(stock, r#""$GIT" notes --ref=bin merge -s union binother; "$STOCK" notes --ref=bin show HEAD | od -c"#);
}

#[test]
fn a_custom_driver_runs_from_the_top_with_its_placeholders() {
    let Some(stock) = stock_git() else { return };
    let t = Twin::new("custom");
    let attr = "echo '* merge=my' > .gitattributes; ";
    t.same(
        stock,
        &format!(
            r#"{attr}"$STOCK" config merge.my.driver 'test -f %O && test -f %A && test -f %B && echo "%L %P %S %X %Y %%" >&2; cat %B %O > %A'; cd sub && "$GIT" notes merge other"#
        ),
    );
    t.same(stock, &format!(r#"{attr}"$STOCK" config merge.my.driver 'printf x > %A; exit 1'; "$GIT" notes merge other"#));
    t.same(stock, &format!(r#"{attr}"$STOCK" config merge.my.driver 'rm %A'; "$GIT" notes merge other"#));
    t.same(stock, &format!(r#"{attr}"$STOCK" config merge.my.driver 'exit 200'; "$GIT" notes merge other"#));
    t.same(stock, &format!(r#"{attr}"$STOCK" config merge.my.name 'nothing to run'; "$GIT" notes merge other"#));
    t.same(stock, r#""$STOCK" config merge.default my; "$STOCK" config merge.my.driver 'echo def > %A'; "$GIT" notes merge other"#);
}

#[test]
fn a_driver_killed_by_a_signal_dies_after_earlier_conflicts() {
    let Some(stock) = stock_git() else { return };
    let t = Twin::new("signal");
    // The object that sorts second gets a driver of its own, which dies.
    t.same(
        stock,
        r#"second=$("$STOCK" notes list | cut -d' ' -f2 | sort | tail -1); printf '* merge=my\n%s merge=die\n' $second > .gitattributes; "$STOCK" config merge.my.driver 'echo m > %A'; "$STOCK" config merge.die.driver 'kill -9 $$'; "$GIT" notes merge other"#,
    );
}

#[test]
fn cat_sort_uniq_fails_over_an_unreadable_note() {
    let Some(stock) = stock_git() else { return };
    let t = Twin::new("csu");
    // `refs/notes/other` gets a note for HEAD whose blob does not exist.
    let missing = r#"h=$("$STOCK" rev-parse HEAD); tree=$(printf "100644 blob 1111111111111111111111111111111111111111\t$h\n" | "$STOCK" mktree --missing); "$STOCK" update-ref refs/notes/other $("$STOCK" commit-tree -p refs/notes/other -m x $tree); "#;
    t.same(stock, &format!(r#"{missing}"$GIT" notes merge -s cat_sort_uniq other"#));
    t.same(stock, &format!(r#"{missing}"$GIT" notes merge -s union other; "$STOCK" notes show HEAD"#));
    t.same(
        stock,
        &format!(
            r#"{missing}printf 'HEAD HEAD~1\nHEAD~1 HEAD\n' | "$GIT" -c notes.rewriteRef=refs/notes/other -c notes.rewriteMode=cat_sort_uniq notes copy --for-rewrite=amend; "$STOCK" log --format=%s refs/notes/other -2"#
        ),
    );
}

#[test]
fn copy_combines_a_missing_source_and_names_the_operands() {
    let Some(stock) = stock_git() else { return };
    let t = Twin::new("copy");
    let unnoted = r#""$STOCK" commit -q --allow-empty -m c3; "#;
    t.same(
        stock,
        &format!(
            r#"{unnoted}printf 'b\na\nb\n' | "$STOCK" notes add -f -F - HEAD~1; echo 'HEAD HEAD~1' | "$GIT" -c notes.rewriteRef=refs/notes/commits -c notes.rewriteMode=cat_sort_uniq notes copy --for-rewrite=amend; "$STOCK" notes show HEAD~1"#
        ),
    );
    t.same(stock, &format!(r#"{unnoted}echo 'HEAD HEAD~1' | "$GIT" notes copy --stdin; "$STOCK" notes list"#));
    t.same(stock, &format!(r#"{unnoted}echo 'HEAD HEAD~1' | "$GIT" notes copy --stdin -f; "$STOCK" notes list"#));
}
