//! A switch between two identical trees in a sparse checkout, against stock git.
//!
//! `merge_working_tree()` runs `unpack_trees()` whatever the two trees are
//! (builtin/checkout.c:916), and with a sparse checkout on its sparse loops re-judge every
//! kept entry (unpack-trees.c:2035-2075): a cone widened without a reapply writes the entries
//! it takes back in, and one narrowed over a modified file warns that it was left. Only the
//! three-word `checkout -b <new>` and an operand-less `switch -c <new>` skip
//! `merge_working_tree()` (`only_merge_on_switching_branches`, builtin/checkout.c:1206-1207).
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

/// A worktree edit made in each side's repository before the command runs.
#[derive(Clone, Copy, Debug)]
enum Edit {
    /// The cone gains `/out/` without a reapply.
    Widen,
    /// The patterns become `/top` alone without a reapply, and `in/b` is modified.
    NarrowModified,
    /// No sparse checkout at all; `in/a` is modified.
    NotSparse,
}

/// `main` holds `in/a in/b out/c out/d out/k top`, and `same` points at the same commit. The
/// index is refreshed a second after the files are written, so no entry is racily clean.
fn build(stock: &str, dir: &Path, home: &Path, edit: Edit) {
    std::fs::create_dir_all(dir).unwrap();
    ok(stock, dir, home, &["init", "-q", "-b", "main", "."]);
    for p in ["in/a", "in/b", "out/c", "out/d", "out/k", "top"] {
        write(dir, p, &format!("{p}\n"));
    }
    ok(stock, dir, home, &["add", "."]);
    ok(stock, dir, home, &["-c", "maintenance.auto=false", "commit", "-q", "-m", "one"]);
    ok(stock, dir, home, &["branch", "same"]);
    if !matches!(edit, Edit::NotSparse) {
        ok(stock, dir, home, &["sparse-checkout", "set", "in"]);
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
        Edit::NotSparse => write(dir, "in/a", "M\n"),
    }
}

/// Everything observable after `args` ran on `side` with `bin`.
fn observe(root: &Path, side: &str, bin: &str, stock: &str, edit: Edit, args: &[&str]) -> String {
    let dir = root.join(side);
    let _ = std::fs::remove_dir_all(&dir);
    build(stock, &dir, root, edit);
    let out = git(bin, &dir, root, args);
    let mut seen = format!(
        "stdout:\n{}stderr:\n{}rc: {:?}\n",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
        out.status.code()
    );
    seen += &stdout(stock, &dir, root, &["ls-files", "-t", "-s"]);
    seen += &stdout(stock, &dir, root, &["status", "--porcelain", "--branch"]);
    seen += &files(&dir);
    seen
}

fn assert_same(edit: Edit, args: &[&str]) {
    let Some(stock) = stock_git() else { return };
    let root = Root(std::env::temp_dir().join(format!("zvcs-sparse-same-tree-{edit:?}-{}", std::process::id())));
    let s = observe(&root.0, "stock", stock, stock, edit, args);
    let z = observe(&root.0, "zvcs", BIN, stock, edit, args);
    assert_eq!(z, s, "`git {}` after {edit:?} diverges from stock", args.join(" "));
}

/// Every spelling of a switch whose two trees are the same.
const SAME_TREE: &[&[&str]] = &[
    &["checkout", "main"],
    &["checkout", "-m", "main"],
    &["checkout", "same"],
    &["checkout", "HEAD"],
    &["checkout"],
    &["checkout", "-m"],
    &["checkout", "--detach"],
    &["checkout", "-B", "nb"],
    &["checkout", "-b", "nb", "main"],
    &["checkout", "--orphan", "nb"],
    &["switch", "main"],
    &["switch", "same"],
    &["switch", "-m", "same"],
    &["switch", "--detach", "main"],
    &["switch", "-c", "nb", "main"],
    // `only_merge_on_switching_branches`: no `merge_working_tree()`, so no sparse loops.
    &["checkout", "-b", "nb"],
    &["switch", "-c", "nb"],
];

/// A widened cone writes `out/c`, `out/d` and `out/k` and clears their bits.
#[test]
fn a_widened_cone_is_applied_by_a_same_tree_switch() {
    for args in SAME_TREE {
        assert_same(Edit::Widen, args);
    }
}

/// A narrowed pattern set takes `in/a` out and warns about the modified `in/b`, except under
/// `-m`, whose first `merge_working_tree()` is quiet.
#[test]
fn a_narrowed_pattern_set_is_applied_by_a_same_tree_switch() {
    for args in SAME_TREE {
        assert_same(Edit::NarrowModified, args);
    }
}

/// Outside a sparse checkout a same-tree switch moves nothing and lists the local change.
#[test]
fn a_same_tree_switch_outside_a_sparse_checkout_only_lists() {
    for args in SAME_TREE {
        assert_same(Edit::NotSparse, args);
    }
}
