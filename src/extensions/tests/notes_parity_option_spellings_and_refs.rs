//! `git notes` subcommand options spelled the way `parse_options()` accepts
//! them, and the notes ref moved the way `refs_update_ref()` moves it.
//!
//! * Each subcommand's table is parsed by `parse_options()`: clumped short
//!   options (`-fm x`, `-nv`, `-sours`), unique abbreviations (`--mess`,
//!   `--for-r=`, `--str`), `--no-<name>` of every negatable entry (`--no-force`,
//!   `--no-allow-empty`, `--no-stdin`, `--no-strategy`), ``option `x' takes no
//!   value`` for `--stripspace=1`, ambiguous abbreviations (`--re`), and
//!   `--end-of-options` ending the options as `--` does (parse-options.c:
//!   426-461, 519-594, 1113-1124). `OPT__FORCE` is in `add()`'s table only
//!   (builtin/notes.c:488-513, :667-691), so `append -f` is an unknown switch.
//! * `commit_notes()` and `merge()` call `refs_update_ref()` with no old value
//!   and no `REF_NO_DEREF` (notes-utils.c:64-66, builtin/notes.c:979-981): a
//!   symbolic notes ref moves its referent, and a bad ref name dies with
//!   `refusing to update ref with bad name`.
//! * `init_notes()` dies when the notes ref does not peel to a tree
//!   (notes.c:1048-1050).
//! * `commit --amend` exports its author (builtin/commit.c:686-688), so the
//!   notes commit `notes.rewriteRef` writes carries the amended commit's author.
//! * A conflicting notes merge refuses to start while another worktree is
//!   merging into the same ref (`find_shared_symref()`, builtin/notes.c:992-997).
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

/// Four commits, notes on two of them and on a blob, a second notes ref
/// `refs/notes/other`, and `refs/notes/theirs` diverging from
/// `refs/notes/commits` so a manual merge conflicts.
const FIXTURE: &str = r#"
set -e
t=1700000000
c() { t=$((t+3600)); GIT_AUTHOR_DATE="@$t +0000" GIT_COMMITTER_DATE="@$t +0000" "$GIT" -c maintenance.auto=false commit -q -m "$1"; }
N() { t=$((t+60)); GIT_AUTHOR_DATE="@$t +0000" GIT_COMMITTER_DATE="@$t +0000" "$GIT" notes "$@"; }
"$GIT" init -q -b main .
for i in 1 2 3 4; do echo $i > f$i; "$GIT" add f$i; c c$i; done
N add -m "c4" HEAD
N add -m "c3" HEAD~1
N add -m "blob" HEAD:f1
N --ref=other add -m "other" HEAD
"$GIT" update-ref refs/notes/base refs/notes/commits
N add -f -m "c4 ours" HEAD
"$GIT" update-ref refs/notes/ours refs/notes/commits
"$GIT" update-ref refs/notes/commits refs/notes/base
N add -f -m "c4 theirs" HEAD
"$GIT" update-ref refs/notes/theirs refs/notes/commits
"$GIT" update-ref refs/notes/commits refs/notes/ours
"$GIT" update-ref -d refs/notes/ours
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
        let root = std::env::temp_dir().join(format!("zvcs-notes-spell-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
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

    /// Run `script` under `sh` with `$GIT` the binary for `side`; the side's own
    /// directory is spelled `<D>` in what comes back.
    fn sh(&self, bin: &str, side: &str, script: &str) -> (String, String, i32) {
        let dir = self.root.join(side);
        let out = env(&mut Command::new("sh"), &self.root)
            .args(["-c", script])
            .env("GIT", bin)
            .env("STOCK", stock_git().unwrap())
            .current_dir(&dir)
            .output()
            .unwrap();
        let dir = dir.to_string_lossy().into_owned();
        let norm = |b: &[u8]| String::from_utf8_lossy(b).replace(&dir, "<D>");
        (norm(&out.stdout), norm(&out.stderr), out.status.code().unwrap_or(-1))
    }

    /// The script's output and the refs, notes and reflogs it left, read back
    /// with stock git.
    fn same(&self, stock: &str, script: &str) {
        let state = r#"; echo "rc=$?"; "$STOCK" for-each-ref --format='%(refname) %(objectname) %(symref)'; for r in $("$STOCK" for-each-ref --format='%(refname)' refs/notes); do echo "== $r"; "$STOCK" log -1 --format='%H %an %ae %ad%n%B' --date=raw $r; "$STOCK" reflog show --format='%H %gs' $r --; done; cat .git/NOTES_MERGE_REF 2>/dev/null; true"#;
        let script = format!("{script}{state}");
        assert_eq!(self.sh(ZVCS, "zvcs", &script), self.sh(stock, "stock", &script), "{script}");
    }
}

fn env<'a>(cmd: &'a mut Command, root: &Path) -> &'a mut Command {
    cmd.env("HOME", root)
        .env("GIT_EDITOR", "echo edited >>")
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
        .env_remove("XDG_CONFIG_HOME")
}

#[test]
fn add_append_and_edit_parse_their_own_tables() {
    let Some(stock) = stock_git() else { return };
    let t = Twin::new(stock, "msg");
    for sub in ["add", "append", "edit"] {
        for opts in [
            "-fm x",
            "--for -m x",
            "--no-force -m x",
            "--mess=x",
            "--mess x",
            "--allow -m ''",
            "--allow-empty --no-allow-empty -m ''",
            "--ed -m x",
            "-em x",
            "--fil /dev/null --allow",
            "--reu=HEAD:f1",
            "--re=HEAD:f1",
            "--sep -m a -m b",
            "--no-sep -m a -m b",
            "--no-strip -m ' a '",
            "--end-of-options -m a",
            "-m a --end-of-options HEAD~2",
            "-m a --stripspace=1",
            "--allow-empty=1 -m a",
            "--no-e -m a",
            "--no-message -m a",
            "-m",
        ] {
            t.same(stock, &format!(r#""$GIT" notes {sub} {opts} HEAD~2"#));
        }
    }
}

#[test]
fn copy_remove_prune_list_and_merge_parse_their_own_tables() {
    let Some(stock) = stock_git() else { return };
    let t = Twin::new(stock, "other");
    for script in [
        r#""$GIT" notes list --"#,
        r#""$GIT" notes list -- HEAD"#,
        r#""$GIT" notes show --end-of-options HEAD"#,
        r#""$GIT" notes get-ref --"#,
        r#""$GIT" notes prune -nv"#,
        r#""$GIT" notes prune --dry --verb"#,
        r#""$GIT" notes prune --dry-run --no-dry-run"#,
        r#""$GIT" notes remove --ign nosuch"#,
        r#""$GIT" notes remove --no-ignore-missing nosuch"#,
        r#"echo HEAD | "$GIT" notes remove --std --no-stdin HEAD~1"#,
        r#""$GIT" notes remove -- HEAD"#,
        r#""$GIT" notes copy --for HEAD HEAD~2"#,
        r#""$GIT" notes copy --no-force HEAD HEAD~1"#,
        r#"echo "$("$STOCK" rev-parse HEAD) $("$STOCK" rev-parse HEAD~3)" | "$GIT" notes copy --for-r=amend"#,
        r#""$GIT" notes copy -- HEAD HEAD~3"#,
        r#""$GIT" notes merge --strat=ours theirs"#,
        r#""$GIT" notes merge --str ours theirs"#,
        r#""$GIT" notes merge -sours theirs"#,
        r#""$GIT" notes merge --verb -s theirs theirs"#,
        r#""$GIT" notes merge --no-strategy -s ours theirs"#,
        r#""$GIT" notes merge --"#,
        r#""$GIT" notes merge --end-of-options theirs; "$GIT" notes merge --abort"#,
        r#""$GIT" notes merge --no-commit theirs"#,
    ] {
        t.same(stock, script);
    }
}

#[test]
fn a_symbolic_notes_ref_moves_its_referent() {
    let Some(stock) = stock_git() else { return };
    let t = Twin::new(stock, "symref");
    t.same(stock, r#""$STOCK" symbolic-ref refs/notes/commits refs/notes/other; "$GIT" notes add -m x HEAD~2"#);
    t.same(stock, r#""$GIT" notes remove HEAD"#);
    t.same(stock, r#""$GIT" notes merge -s theirs theirs"#);
}

#[test]
fn a_notes_ref_that_is_no_tree_or_no_ref_name_dies() {
    let Some(stock) = stock_git() else { return };
    let t = Twin::new(stock, "bad");
    t.same(stock, r#""$GIT" notes --ref='bad..name' add -m x HEAD"#);
    t.same(stock, r#""$STOCK" update-ref refs/notes/blob HEAD:f1; "$GIT" notes --ref=blob list"#);
    t.same(stock, r#""$GIT" notes --ref=blob add -m x HEAD"#);
}

#[test]
fn amend_rewrites_notes_under_the_amended_commits_author() {
    let Some(stock) = stock_git() else { return };
    let t = Twin::new(stock, "amend");
    t.same(stock, r#""$GIT" -c notes.rewriteRef=refs/notes/commits -c maintenance.auto=false commit --amend -q -m amended"#);
    t.same(stock, r#""$GIT" -c notes.rewriteRef=refs/notes/commits -c maintenance.auto=false commit --amend -q --reset-author -m again"#);
}

#[test]
fn a_second_worktree_cannot_merge_into_a_ref_already_being_merged() {
    let Some(stock) = stock_git() else { return };
    let t = Twin::new(stock, "wt");
    t.same(
        stock,
        r#""$STOCK" worktree add -q -b wtb "$PWD-wt" 2>/dev/null; "$GIT" notes merge theirs; "$GIT" -C "$PWD-wt" notes merge theirs; ls "$("$STOCK" rev-parse --git-common-dir)"/worktrees/*/NOTES_MERGE_* 2>/dev/null | wc -l | tr -d " "; rm -rf "$PWD-wt""#,
    );
}
