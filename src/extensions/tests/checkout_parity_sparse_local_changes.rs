//! The sparse-checkout corners of a switch and a `read-tree` that the in-memory index and
//! `verify_absent()` decide, against stock git:
//!
//! * `show_local_changes()` runs `run_diff_index()` on the index `unpack_trees()` left in
//!   memory (builtin/checkout.c:930-931), not on one read back from disk: an entry the refresh
//!   marked `CE_UPTODATE` is not `lstat()`ed (diff-lib.c:324), so a widened cone that clears the
//!   bit of a file it does not write lists nothing, and neither does a new skip-worktree entry
//!   over an untracked directory;
//! * `read-tree -n -u` with two or three trees still runs the sparse loops and prints their
//!   warnings before it stops short of the worktree (unpack-trees.c:442-446);
//! * a new entry over an untracked file in a sparse checkout is not refused by `merged_entry()`
//!   but left to the sparse loop (unpack-trees.c:2569-2588), under `read-tree -m` too;
//! * a directory where the sparse loop is to write a file goes to `verify_clean_subdirectory()`
//!   (unpack-trees.c:2323-2396), whose `ERROR_NOT_UPTODATE_DIR` is an `error()` under
//!   `read-tree` and nothing under the porcelain.
//!
//! Each side gets its own repository built by stock git with the dates pinned, the same edit,
//! and one command run by stock in one and by this binary in the other; stdout, stderr, the
//! exit code, `ls-files -t -s`, `status --porcelain` and the worktree files are compared.
//!
//! Skipped when no stock git is available.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Root(PathBuf);

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
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

#[derive(Clone, Copy, PartialEq)]
enum Layout {
    /// `sparse-checkout set in`.
    Cone,
    /// `sparse-checkout set --no-cone /in/ /top`.
    NonCone,
}

/// A worktree edit made in each side's repository before the command runs.
#[derive(Clone, Copy, Debug)]
enum Edit {
    /// The patterns gain `/out/` without a reapply.
    Widen,
    /// The patterns lose `in/` without a reapply, and `in/b` is modified.
    NarrowModified,
    /// An untracked `in/f`, which `other` adds inside the patterns.
    UntrackedFile,
    /// An untracked `in/f/u`: a directory where `other` adds the file `in/f`.
    UntrackedDirInCone,
    /// An untracked `out/e/u`: a directory where `other` adds `out/e` outside the patterns.
    UntrackedDirOutside,
}

/// `main` holds `in/a in/b out/c out/d out/k top`; `other` changes `in/a` and `out/c`, drops
/// `out/d` and adds `out/e` and `in/f`; `main` then changes `top`. The index is refreshed a
/// second after the files are written, so no entry is racily clean.
fn build(stock: &str, dir: &Path, home: &Path, layout: Layout, edit: Edit) {
    std::fs::create_dir_all(dir).unwrap();
    ok(stock, dir, home, &["init", "-q", "-b", "main", "."]);
    for p in ["in/a", "in/b", "out/c", "out/d", "out/k", "top"] {
        write(dir, p, &format!("{p}\n"));
    }
    ok(stock, dir, home, &["add", "."]);
    ok(stock, dir, home, &["commit", "-q", "-m", "one"]);
    ok(stock, dir, home, &["checkout", "-q", "-b", "other"]);
    write(dir, "in/a", "a2\n");
    write(dir, "out/c", "c2\n");
    write(dir, "out/e", "e\n");
    write(dir, "in/f", "f\n");
    ok(stock, dir, home, &["rm", "-q", "out/d"]);
    ok(stock, dir, home, &["add", "-A"]);
    ok(stock, dir, home, &["commit", "-q", "-m", "two"]);
    ok(stock, dir, home, &["checkout", "-q", "main"]);
    write(dir, "top", "top2\n");
    ok(stock, dir, home, &["commit", "-q", "-a", "-m", "three"]);
    match layout {
        Layout::Cone => ok(stock, dir, home, &["sparse-checkout", "set", "in"]),
        Layout::NonCone => ok(stock, dir, home, &["sparse-checkout", "set", "--no-cone", "/in/", "/top"]),
    }
    std::thread::sleep(std::time::Duration::from_millis(1100));
    ok(stock, dir, home, &["update-index", "-q", "--refresh"]);
    let patterns = dir.join(".git/info/sparse-checkout");
    match edit {
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
        Edit::UntrackedFile => write(dir, "in/f", "U\n"),
        Edit::UntrackedDirInCone => write(dir, "in/f/u", "U\n"),
        Edit::UntrackedDirOutside => write(dir, "out/e/u", "U\n"),
    }
}

/// Everything observable after `args` ran on `side` with `bin`.
fn observe(root: &Path, side: &str, bin: &str, stock: &str, layout: Layout, edit: Edit, args: &[&str]) -> String {
    let dir = root.join(side);
    let _ = std::fs::remove_dir_all(&dir);
    build(stock, &dir, root, layout, edit);
    let out = git(bin, &dir, root, args);
    let mut seen = format!(
        "stdout:\n{}stderr:\n{}rc: {:?}\n",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
        out.status.code()
    );
    seen += &stdout(stock, &dir, root, &["ls-files", "-t", "-s"]);
    seen += &stdout(stock, &dir, root, &["status", "--porcelain"]);
    seen += &files(&dir);
    seen
}

fn assert_same(tag: &str, layout: Layout, edit: Edit, args: &[&str]) {
    let Some(stock) = stock_git() else { return };
    let root = Root(std::env::temp_dir().join(format!("zvcs-sparse-local-{tag}-{}", std::process::id())));
    let s = observe(&root.0, "stock", stock, stock, layout, edit, args);
    let z = observe(&root.0, "zvcs", BIN, stock, layout, edit, args);
    assert_eq!(z, s, "`git {}` after {edit:?} diverges from stock", args.join(" "));
}

/// A widened cone clears the bit of `out/k` without writing it; the refreshed entry is
/// `CE_UPTODATE`, so the listing does not call it deleted.
#[test]
fn a_widened_cone_lists_no_local_changes() {
    for (layout, tag) in [(Layout::Cone, "widen-cone"), (Layout::NonCone, "widen-noncone")] {
        for args in [&["checkout", "other"][..], &["switch", "other"], &["checkout", "HEAD~1"]] {
            assert_same(tag, layout, Edit::Widen, args);
        }
    }
}

/// A new entry outside the patterns over an untracked directory is a skip-worktree entry the
/// listing does not look at.
#[test]
fn a_new_skip_worktree_entry_over_a_directory_is_not_listed() {
    for (layout, tag) in [(Layout::Cone, "dirout-cone"), (Layout::NonCone, "dirout-noncone")] {
        for args in [&["checkout", "other"][..], &["switch", "other"]] {
            assert_same(tag, layout, Edit::UntrackedDirOutside, args);
        }
    }
}

/// `read-tree -n -u` over two and three trees prints the sparse loops' warnings and writes
/// nothing.
#[test]
fn read_tree_dry_run_prints_the_sparse_warnings() {
    for edit in [Edit::NarrowModified, Edit::UntrackedFile, Edit::UntrackedDirInCone] {
        for args in [
            &["read-tree", "-n", "-u", "-m", "HEAD", "other"][..],
            &["read-tree", "-n", "-u", "-m", "HEAD~1", "HEAD", "other"],
            &["read-tree", "-n", "-u", "-m", "other"],
        ] {
            assert_same("dry-run", Layout::Cone, edit, args);
        }
    }
}

/// `read-tree -m -u` lets the sparse loop judge a new entry over an untracked file or
/// directory: the orphaned warning for a file, `verify_clean_subdirectory()`'s error for a
/// directory with something untracked in it, and the entry written either way.
#[test]
fn read_tree_merge_leaves_new_entries_to_the_sparse_loop() {
    for (layout, tag) in [(Layout::Cone, "rt-cone"), (Layout::NonCone, "rt-noncone")] {
        for edit in [Edit::UntrackedFile, Edit::UntrackedDirInCone] {
            for args in [
                &["read-tree", "-u", "-m", "HEAD", "other"][..],
                &["read-tree", "-u", "-m", "other"],
                &["read-tree", "-u", "-m", "HEAD~1", "HEAD", "other"],
            ] {
                assert_same(tag, layout, edit, args);
            }
        }
    }
}

/// Under the porcelain, `verify_clean_subdirectory()`'s refusal goes on a list nothing prints.
#[test]
fn a_directory_in_the_way_is_silent_under_the_porcelain() {
    for args in [
        &["checkout", "other"][..],
        &["switch", "other"],
        &["merge", "other"],
        &["switch", "--discard-changes", "other"],
        &["reset", "--merge", "other"],
    ] {
        assert_same("dirin", Layout::Cone, Edit::UntrackedDirInCone, args);
    }
}

