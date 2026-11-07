//! The sparse-checkout half of `unpack_trees()` (unpack-trees.c:1974-1976, :2035-2075) under
//! every verb that moves the worktree from one tree to another, in cone and non-cone sparse
//! checkouts, against stock git.
//!
//! The sparse loops run over the whole result, not only the paths the merge touched:
//!
//! * a present file outside the patterns — its bit cleared on read by
//!   `clear_skip_worktree_from_present_files()` — is taken out of the worktree again by
//!   `apply_sparse_checkout()`, or kept with the `not up to date` warning when it is modified
//!   and the merge did not give it `CE_UPDATE`;
//! * an entry the patterns now take in is written, and an entry they now leave out removed;
//! * a new entry over an untracked file in the cone is a `WARNING_SPARSE_ORPHANED_NOT_OVERWRITTEN`
//!   rather than a refusal, because `merged_entry()` set `CE_NEW_SKIP_WORKTREE` before
//!   `verify_absent()` looked;
//! * the warnings come as one porcelain block under `checkout` and `merge`, as one `error:` per
//!   path under `read-tree`, `reset` and `checkout -f`/`switch --discard-changes`, and not at
//!   all on the first, `quiet` attempt of `checkout -m`;
//! * a re-applied autostash puts each path it changed back with no stat data
//!   (`unstage_changes_unless_new()`, builtin/stash.c:529-640), writing a skip-worktree one out
//!   first, and a conflicted one outside the patterns is written by
//!   `record_conflicted_index_entries()` (merge-ort.c:4714-4726).
//!
//! Each case builds one repository with stock git, copies it, makes the same edit in both
//! copies, runs one command with stock in one and this binary in the other, and compares
//! stdout, stderr, the exit code, `ls-files -t -v`, `ls-files --debug` without the
//! timestamps and device numbers, the worktree files, `status --porcelain` and `HEAD`.
//!
//! Skipped when no stock git is available.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

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

/// Every regular file under `dir` outside `.git`, sorted, with its content.
fn files(dir: &Path) -> String {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let path = e.path();
            if path.file_name().is_some_and(|n| n == ".git") {
                continue;
            }
            if e.file_type().unwrap().is_dir() {
                walk(base, &path, out);
            } else {
                let rel = path.strip_prefix(base).unwrap().to_string_lossy().into_owned();
                out.push((rel, std::fs::read_to_string(&path).unwrap_or_default()));
            }
        }
    }
    let mut all = Vec::new();
    walk(dir, dir, &mut all);
    all.sort();
    all.into_iter().map(|(p, body)| format!("{p}: {body}")).collect()
}

#[derive(Clone, Copy)]
enum Layout {
    /// `sparse-checkout set in`.
    Cone,
    /// `sparse-checkout set --no-cone /in/ /top`.
    NonCone,
}

/// A worktree edit made identically in both copies before the command runs.
#[derive(Clone, Copy, Debug)]
enum Edit {
    /// `out/c` and `out/k`, outside the patterns, written back with their committed content.
    PresentClean,
    /// `out/c` and `out/k` written back modified.
    PresentModified,
    /// The patterns gain `/out/` without a reapply.
    Widen,
    /// The patterns lose `in/` without a reapply, and `in/b` is modified.
    NarrowModified,
    /// `out/k` staged with `add --sparse`, so it is tracked outside the patterns without the bit.
    AddSparse,
    /// An untracked `in/f`, which `other` adds inside the patterns.
    UntrackedInCone,
}

impl Edit {
    fn apply(self, stock: &str, dir: &Path, home: &Path) {
        let patterns = dir.join(".git/info/sparse-checkout");
        match self {
            Edit::PresentClean => {
                write(dir, "out/c", "out/c\n");
                write(dir, "out/k", "out/k\n");
            }
            Edit::PresentModified => {
                write(dir, "out/c", "X\n");
                write(dir, "out/k", "Y\n");
            }
            Edit::Widen => {
                let mut text = std::fs::read_to_string(&patterns).unwrap();
                text.push_str("/out/\n");
                std::fs::write(&patterns, text).unwrap();
            }
            Edit::NarrowModified => {
                ok(stock, dir, home, &["config", "--worktree", "core.sparseCheckoutCone", "false"]);
                std::fs::write(&patterns, "/top\n").unwrap();
                write(dir, "in/b", "M\n");
            }
            Edit::AddSparse => {
                write(dir, "out/k", "K2\n");
                ok(stock, dir, home, &["add", "--sparse", "out/k"]);
            }
            Edit::UntrackedInCone => write(dir, "in/f", "U\n"),
        }
    }
}

impl Fixture {
    /// `main` holds `in/a in/b out/c out/d out/k top`; `other` changes `in/a` and `out/c`,
    /// drops `out/d` and adds `out/e` and `in/f`; `main` then changes `top`. The index is
    /// refreshed a second after the files are written, so no entry is racily clean.
    fn new(stock: &str, layout: Layout, tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-sparse-unpack-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let base = root.join("base");
        std::fs::create_dir_all(&base).unwrap();
        let f = Fixture { root };
        let home = f.root.as_path();
        ok(stock, &base, home, &["init", "-q", "-b", "main", "."]);
        for p in ["in/a", "in/b", "out/c", "out/d", "out/k", "top"] {
            write(&base, p, &format!("{p}\n"));
        }
        ok(stock, &base, home, &["add", "."]);
        ok(stock, &base, home, &["commit", "-q", "-m", "one"]);
        ok(stock, &base, home, &["checkout", "-q", "-b", "other"]);
        write(&base, "in/a", "a2\n");
        write(&base, "out/c", "c2\n");
        write(&base, "out/e", "e\n");
        write(&base, "in/f", "f\n");
        ok(stock, &base, home, &["rm", "-q", "out/d"]);
        ok(stock, &base, home, &["add", "-A"]);
        ok(stock, &base, home, &["commit", "-q", "-m", "two"]);
        ok(stock, &base, home, &["checkout", "-q", "main"]);
        write(&base, "top", "top2\n");
        ok(stock, &base, home, &["commit", "-q", "-a", "-m", "three"]);
        match layout {
            Layout::Cone => ok(stock, &base, home, &["sparse-checkout", "set", "in"]),
            Layout::NonCone => ok(stock, &base, home, &["sparse-checkout", "set", "--no-cone", "/in/", "/top"]),
        }
        std::thread::sleep(std::time::Duration::from_millis(1100));
        ok(stock, &base, home, &["update-index", "-q", "--refresh"]);
        f
    }

    /// Everything observable after `args` ran in a fresh copy of the base with `edit` made.
    fn observe(&self, side: &str, bin: &str, stock: &str, edit: Edit, args: &[&str]) -> String {
        let dir = self.root.join(side);
        let _ = std::fs::remove_dir_all(&dir);
        let status = Command::new("cp").arg("-Rp").arg(self.root.join("base")).arg(&dir).status().unwrap();
        assert!(status.success());
        edit.apply(stock, &dir, &self.root);
        let out = git(bin, &dir, &self.root, args);
        let mut seen = format!(
            "stdout:\n{}stderr:\n{}rc: {:?}\n",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
            out.status.code()
        );
        seen += &stdout(stock, &dir, &self.root, &["ls-files", "-t", "-v"]);
        // Size, uid and flags; ctime, mtime, dev and ino are the filesystem's.
        let debug = stdout(stock, &dir, &self.root, &["ls-files", "--debug"]);
        seen += &debug
            .lines()
            .filter(|l| !l.starts_with("  ctime:") && !l.starts_with("  mtime:") && !l.starts_with("  dev:"))
            .map(|l| format!("{l}\n"))
            .collect::<String>();
        seen += &files(&dir);
        seen += &stdout(stock, &dir, &self.root, &["status", "--porcelain"]);
        seen += &stdout(stock, &dir, &self.root, &["rev-parse", "--abbrev-ref", "HEAD"]);
        seen
    }

    fn assert_same(&self, stock: &str, edit: Edit, args: &[&str]) {
        let s = self.observe("stock", stock, stock, edit, args);
        let z = self.observe("zvcs", BIN, stock, edit, args);
        assert_eq!(z, s, "`git {}` after {edit:?} diverges from stock", args.join(" "));
    }
}

/// The verbs whose sparse loops run, with each one's message style.
const VERBS: [&[&str]; 10] = [
    &["checkout", "other"],
    &["checkout", "HEAD~1"],
    &["checkout", "-f", "other"],
    &["switch", "--discard-changes", "other"],
    &["reset", "--hard", "other"],
    &["reset", "--hard"],
    &["reset", "--merge", "other"],
    &["read-tree", "-m", "-u", "HEAD", "other"],
    &["read-tree", "--reset", "-u", "other"],
    &["merge", "other"],
];

/// A present file outside the patterns is taken out of the worktree again when it is clean,
/// and left with the `not up to date` warning when it is modified and nothing rewrites it.
#[test]
fn present_files_outside_the_patterns_are_resparsified() {
    let Some(stock) = stock_git() else { return };
    for (layout, tag) in [(Layout::Cone, "present-cone"), (Layout::NonCone, "present-noncone")] {
        let f = Fixture::new(stock, layout, tag);
        for edit in [Edit::PresentClean, Edit::PresentModified] {
            for args in VERBS {
                f.assert_same(stock, edit, args);
            }
        }
    }
}

/// Patterns edited without a reapply: what they take in is written, what they leave out is
/// removed or, modified, kept with the warning; and a tracked file `add --sparse` put outside
/// them is taken out of the worktree with its bit set.
#[test]
fn changed_patterns_and_sparse_adds_are_applied_to_untouched_entries() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, Layout::NonCone, "patterns-noncone");
    for edit in [Edit::Widen, Edit::NarrowModified, Edit::AddSparse] {
        for args in VERBS {
            f.assert_same(stock, edit, args);
        }
    }
    let f = Fixture::new(stock, Layout::Cone, "patterns-cone");
    for edit in [Edit::NarrowModified, Edit::AddSparse] {
        for args in VERBS {
            f.assert_same(stock, edit, args);
        }
    }
}

/// A new entry inside the patterns over an untracked file: overwritten with the orphaned
/// warning by a switch, a merge and `--discard-changes`, silently by `checkout -f`.
#[test]
fn an_untracked_file_under_a_new_entry_in_the_cone_is_a_warning() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, Layout::Cone, "untracked");
    for args in VERBS {
        f.assert_same(stock, Edit::UntrackedInCone, args);
    }
}

/// `checkout -m`: the first attempt is `quiet`, so a switch that needs no autostash shows no
/// sparse warning; an autostash that conflicts outside the patterns writes the conflicted file.
#[test]
fn checkout_merge_is_quiet_first_and_writes_conflicts_outside_the_patterns() {
    let Some(stock) = stock_git() else { return };
    for (layout, tag) in [(Layout::Cone, "merge-cone"), (Layout::NonCone, "merge-noncone")] {
        let f = Fixture::new(stock, layout, tag);
        for edit in [Edit::PresentModified, Edit::NarrowModified, Edit::UntrackedInCone] {
            f.assert_same(stock, edit, &["checkout", "-m", "other"]);
        }
    }
}

/// A clean autostash re-apply leaves the paths it changed unstaged with no stat data, outside
/// any sparse checkout too.
#[test]
fn checkout_merge_autostash_unstages_with_a_zero_stat() {
    let Some(stock) = stock_git() else { return };
    let root = std::env::temp_dir().join(format!("zvcs-sparse-unpack-autostash-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let base = root.join("base");
    std::fs::create_dir_all(&base).unwrap();
    let f = Fixture { root };
    let home = f.root.as_path();
    ok(stock, &base, home, &["init", "-q", "-b", "main", "."]);
    write(&base, "x", "1\n2\n3\n4\n5\n");
    write(&base, "k", "k\n");
    ok(stock, &base, home, &["add", "."]);
    ok(stock, &base, home, &["commit", "-q", "-m", "one"]);
    ok(stock, &base, home, &["checkout", "-q", "-b", "other"]);
    write(&base, "x", "one\n2\n3\n4\n5\n");
    ok(stock, &base, home, &["commit", "-q", "-a", "-m", "two"]);
    ok(stock, &base, home, &["checkout", "-q", "main"]);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    ok(stock, &base, home, &["update-index", "-q", "--refresh"]);
    let edit = |dir: &Path| write(dir, "x", "1\n2\n3\n4\nfive\n");
    let observe = |side: &str, bin: &str| {
        let dir = f.root.join(side);
        let status = Command::new("cp").arg("-Rp").arg(&base).arg(&dir).status().unwrap();
        assert!(status.success());
        edit(&dir);
        let out = git(bin, &dir, home, &["checkout", "-m", "other"]);
        let debug = stdout(stock, &dir, home, &["ls-files", "--debug"]);
        format!(
            "stdout:\n{}stderr:\n{}rc: {:?}\n{}{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
            out.status.code(),
            debug
                .lines()
                .filter(|l| !l.starts_with("  ctime:") && !l.starts_with("  mtime:") && !l.starts_with("  dev:"))
                .map(|l| format!("{l}\n"))
                .collect::<String>(),
            files(&dir),
            stdout(stock, &dir, home, &["status", "--porcelain"]),
        )
    };
    let s = observe("stock", stock);
    let z = observe("zvcs", BIN);
    assert_eq!(z, s, "`git checkout -m other` with a clean autostash diverges from stock");
}
