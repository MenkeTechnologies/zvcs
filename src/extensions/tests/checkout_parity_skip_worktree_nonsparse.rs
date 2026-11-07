//! `CE_SKIP_WORKTREE` entries in a repository that is not a sparse checkout — the bit set by
//! `update-index --skip-worktree` with `core.sparseCheckout` unset — under every verb that runs
//! `unpack_trees()` with `o->update`, against stock git.
//!
//! Without a sparse checkout `o->skip_sparse_checkout` is set (unpack-trees.c:1931-1932), so:
//!
//! * `verify_uptodate()` never takes its sparse exit (:2275-2278), and `verify_uptodate_1()`
//!   checks the file of a skip-worktree entry the merge replaces even under `o->reset`
//!   (:2233-2239): a modified one refuses `checkout`, `checkout -m` (whose autostash cannot see
//!   it, sequencer.c:4742-4743), `merge`, `checkout -f`, `reset --hard` and `read-tree --reset -u`;
//! * `merged_entry()` migrates the bit onto the entry it writes (:2616-2617) and no
//!   `apply_sparse_checkout()` takes `CE_UPDATE` away, so `check_updates()` writes the file and
//!   records its stat data (:483-498) while the bit stays;
//! * `deleted_entry()`'s `CE_REMOVE` becomes `CE_WT_REMOVE` (:222-223), so a skip-worktree file
//!   the target drops is removed.
//!
//! Each case builds one repository with stock git, copies it, makes the same worktree edit in
//! both copies, runs one command with stock in one and with this binary in the other, and
//! compares stdout, stderr, the exit code, `ls-files -t -v`, `ls-files --debug` without the
//! filesystem facts, the worktree files, `status --porcelain` and `HEAD`.
//!
//! Skipped when no stock git is available.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// The worktree files every case reports.
const FILES: [&str; 7] = ["a", "b", "d/c", "e", "f", "g", "h"];

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn git(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_INDEX_VERSION")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .output()
        .unwrap()
}

fn ok(bin: &str, dir: &Path, home: &Path, args: &[&str]) {
    let out = git(bin, dir, home, args);
    assert!(out.status.success(), "`{bin} {args:?}` failed: {out:?}");
}

fn stdout(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> String {
    String::from_utf8_lossy(&git(bin, dir, home, args).stdout).into_owned()
}

fn write(dir: &Path, path: &str, body: &str) {
    let full = dir.join(path);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, body).unwrap();
}

/// A worktree edit made identically in both copies before the command runs.
#[derive(Clone, Copy)]
enum Edit {
    None,
    /// `b`, skip-worktree and changed by `other`, is modified.
    ModifyB,
    /// `b` is deleted.
    DeleteB,
    /// `b` and `d/c` (both skip-worktree) and `e` (not) are modified.
    ModifyBAndE,
}

impl Edit {
    fn apply(self, dir: &Path) {
        match self {
            Edit::None => {}
            Edit::ModifyB => write(dir, "b", "X\n"),
            Edit::DeleteB => std::fs::remove_file(dir.join("b")).unwrap(),
            Edit::ModifyBAndE => {
                write(dir, "b", "X\n");
                write(dir, "d/c", "Z\n");
                write(dir, "e", "Y\n");
            }
        }
    }
}

impl Fixture {
    /// `main` holds `a b d/c e f h`; `other` changes `b` and `d/c`, drops `f` and adds `g`;
    /// `main` then changes `e`. `b`, `d/c`, `f` and `h` are marked skip-worktree with no sparse
    /// checkout configured. The index is written a second after the files, so no entry is
    /// racily clean and the stat data both sides write is comparable.
    fn new(stock: &str, tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-skip-worktree-nonsparse-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let base = root.join("base");
        std::fs::create_dir_all(&base).unwrap();
        let f = Fixture { root };
        ok(stock, &base, &f.root, &["init", "-q", "-b", "main", "."]);
        for (p, body) in [("a", "a\n"), ("b", "b\n"), ("d/c", "c\n"), ("e", "e\n"), ("f", "f\n"), ("h", "h\n")] {
            write(&base, p, body);
        }
        ok(stock, &base, &f.root, &["add", "."]);
        ok(stock, &base, &f.root, &["commit", "-q", "-m", "one"]);
        ok(stock, &base, &f.root, &["checkout", "-q", "-b", "other"]);
        write(&base, "b", "b2\n");
        write(&base, "d/c", "c2\n");
        write(&base, "g", "g\n");
        ok(stock, &base, &f.root, &["rm", "-q", "f"]);
        ok(stock, &base, &f.root, &["add", "g"]);
        ok(stock, &base, &f.root, &["commit", "-q", "-a", "-m", "two"]);
        ok(stock, &base, &f.root, &["checkout", "-q", "main"]);
        write(&base, "e", "e2\n");
        ok(stock, &base, &f.root, &["commit", "-q", "-a", "-m", "three"]);
        std::thread::sleep(std::time::Duration::from_millis(1100));
        ok(stock, &base, &f.root, &["update-index", "--skip-worktree", "b", "d/c", "f", "h"]);
        f
    }

    /// Everything observable after `args` ran in a fresh copy of the base with `edit` made.
    fn observe(&self, side: &str, bin: &str, stock: &str, edit: Edit, args: &[&str], index_stat: bool) -> String {
        let dir = self.root.join(side);
        let _ = std::fs::remove_dir_all(&dir);
        let status = Command::new("cp").arg("-Rp").arg(self.root.join("base")).arg(&dir).status().unwrap();
        assert!(status.success());
        edit.apply(&dir);
        let out = git(bin, &dir, &self.root, args);
        let mut seen = format!(
            "stdout:\n{}stderr:\n{}rc: {:?}\n",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
            out.status.code()
        );
        seen += &stdout(stock, &dir, &self.root, &["ls-files", "-t", "-v"]);
        if index_stat {
            // Size and flags; ctime, mtime, dev, ino, uid and gid are the filesystem's.
            let debug = stdout(stock, &dir, &self.root, &["ls-files", "--debug"]);
            seen += &debug
                .lines()
                .filter(|l| !l.starts_with("  ctime:") && !l.starts_with("  mtime:"))
                .filter(|l| !l.starts_with("  dev:") && !l.starts_with("  uid:"))
                .map(|l| format!("{l}\n"))
                .collect::<String>();
        }
        for p in FILES {
            let body = std::fs::read_to_string(dir.join(p)).unwrap_or_else(|_| "MISSING\n".into());
            seen += &format!("{p}: {body}");
        }
        seen += &stdout(stock, &dir, &self.root, &["status", "--porcelain"]);
        seen += &stdout(stock, &dir, &self.root, &["rev-parse", "--abbrev-ref", "HEAD"]);
        seen
    }

    fn assert_same(&self, stock: &str, edit: Edit, args: &[&str]) {
        self.assert_same_with(stock, edit, args, true);
    }

    fn assert_same_with(&self, stock: &str, edit: Edit, args: &[&str], index_stat: bool) {
        let s = self.observe("stock", stock, stock, edit, args, index_stat);
        let z = self.observe("zvcs", BIN, stock, edit, args, index_stat);
        assert_eq!(z, s, "`git {}` diverges from stock", args.join(" "));
    }
}

/// A modified skip-worktree file the switch would replace refuses it, with the porcelain
/// block, however the switch is spelled — and `-m` refuses too, as its autostash sees nothing.
#[test]
fn a_modified_skip_worktree_file_refuses_a_switch() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, "refuse");
    for args in [&["checkout", "other"][..], &["switch", "other"], &["checkout", "-m", "other"], &["merge", "other"]] {
        f.assert_same(stock, Edit::ModifyB, args);
    }
}

/// `o->reset` does not let a skip-worktree entry through: `checkout -f`, `reset --hard` and
/// `read-tree --reset -u` stop on the first one with the plumbing message.
#[test]
fn a_modified_skip_worktree_file_refuses_a_forced_reset() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, "reset");
    for args in [
        &["checkout", "-f", "other"][..],
        &["switch", "--discard-changes", "other"],
        &["reset", "--hard", "other"],
        &["read-tree", "--reset", "-u", "other"],
    ] {
        f.assert_same(stock, Edit::ModifyB, args);
    }
    // A kept entry is `same()` and never checked, nor rewritten.
    f.assert_same(stock, Edit::ModifyB, &["reset", "--hard"]);
}

/// A clean or deleted skip-worktree file the merge changes is checked out, its stat data
/// recorded, and its bit kept; one the target drops is removed.
#[test]
fn a_switch_writes_skip_worktree_files_and_keeps_the_bit() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, "write");
    for edit in [Edit::None, Edit::DeleteB] {
        for args in [
            &["checkout", "other"][..],
            &["checkout", "-f", "other"],
            &["reset", "--hard", "other"],
            &["reset", "--keep", "other"],
            &["reset", "--merge", "other"],
            &["read-tree", "-m", "-u", "HEAD", "other"],
            &["read-tree", "-m", "-u", "other"],
            &["read-tree", "--reset", "-u", "other"],
            &["merge", "other"],
        ] {
            f.assert_same(stock, edit, args);
        }
    }
}

/// `checkout -m` with a visible change stashes it, is refused again by the change the stash
/// could not see, puts the stash back and fails (builtin/checkout.c:1219-1253). The index
/// stat data is left out: re-applying an autostash zeroes the stat of the paths it changed
/// (`unstage_changes_unless_new()`, builtin/stash.c:529-640), which this port does not do on
/// any `checkout -m`.
#[test]
fn checkout_merge_puts_the_autostash_back_when_the_retry_refuses() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, "autostash");
    f.assert_same_with(stock, Edit::ModifyBAndE, &["checkout", "-m", "other"], false);
}
