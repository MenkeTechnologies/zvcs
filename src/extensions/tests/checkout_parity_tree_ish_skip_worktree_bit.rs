//! `git checkout <tree-ish> -- <paths>` on a sparse checkout: `read_tree_some()`
//! runs before the skip-worktree check (builtin/checkout.c:635-636), and
//! `update_some()` keeps the old entry, `CE_SKIP_WORKTREE` included, only when
//! the tree names the same blob and mode (:214-229); any other tree entry
//! replaces it with a fresh `create_ce_flags(0)` one (:210, :231-232). So a
//! sparse path the tree changes is checked out even without
//! `--ignore-skip-worktree-bits`, while one it leaves alone keeps its bit, with
//! the flag too, in the index `write_locked_index()` stores (:701).
//!
//! zvcs read the bit from the index before the tree was laid over it, so
//! `checkout HEAD~ -- out/b` was `did not match`, and with the flag it cleared
//! the bit on every path it wrote.

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn git(bin: &str, dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir.parent().unwrap())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    (
        out.status.code().expect("no signal"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Two commits, `out/b` changed between them and `out/d` not, then a sparse
/// checkout of `in` (cone) or `/in/` + `/top` (non-cone), built by stock git so
/// both sides start from the same index. `sparse.expectFilesOutsideOfPatterns`
/// turns off the reader's `clear_skip_worktree_from_present_files()`
/// (sparse-index.c:684-686), so `ls-files -t` shows the bit as stored on disk.
fn fixture(stock: &str, tag: &str, cone: bool) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap().join("repo");
    for dir in ["in", "out"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    git(stock, &root, &["init", "-q", "-b", "main"]);
    for (p, c) in [("in/a", "a\n"), ("out/b", "b\n"), ("out/d", "d\n"), ("top", "c\n")] {
        std::fs::write(root.join(p), c).unwrap();
    }
    git(stock, &root, &["add", "-A"]);
    git(stock, &root, &["commit", "-q", "-m", "one"]);
    for (p, c) in [("in/a", "a2\n"), ("out/b", "b2\n"), ("top", "c2\n")] {
        std::fs::write(root.join(p), c).unwrap();
    }
    git(stock, &root, &["commit", "-q", "-a", "-m", "two"]);
    if cone {
        git(stock, &root, &["sparse-checkout", "set", "--cone", "in"]);
    } else {
        git(stock, &root, &["sparse-checkout", "set", "--no-cone", "/in/", "/top"]);
    }
    git(stock, &root, &["config", "sparse.expectFilesOutsideOfPatterns", "true"]);
    root
}

/// Exit code, output, the stored skip-worktree bits, the index entries, two
/// `status` runs (the first may rewrite the index) and the sparse files' content.
fn observe(bin: &str, stock: &str, tag: &str, cone: bool, args: &[&str]) -> Vec<String> {
    let dir = fixture(stock, tag, cone);
    let (code, out, err) = git(bin, &dir, args);
    let mut seen = vec![code.to_string(), out, err];
    for probe in [
        &["ls-files", "-t"][..],
        &["ls-files", "--sparse", "-s"],
        &["status", "--porcelain"],
        &["status", "--porcelain"],
        &["ls-files", "-t"],
    ] {
        seen.push(git(stock, &dir, probe).1);
    }
    for p in ["out/b", "out/d"] {
        seen.push(std::fs::read_to_string(dir.join(p)).unwrap_or_else(|_| "<absent>".into()));
    }
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    seen
}

fn compare(tag: &str, cases: &[&[&str]]) {
    let Some(stock) = stock_git() else { return };
    for cone in [true, false] {
        for (n, args) in cases.iter().enumerate() {
            let z = observe(ZVCS, stock, &format!("{tag}-z{n}-{cone}"), cone, args);
            let s = observe(stock, stock, &format!("{tag}-s{n}-{cone}"), cone, args);
            assert_eq!(z, s, "cone={cone} {args:?}: left is zvcs, right is stock");
        }
    }
}

#[test]
fn tree_ish_checkout_reads_the_bit_after_update_some() {
    compare(
        "co-tree-isw",
        &[
            &["checkout", "HEAD~", "--", "out/b"],
            &["checkout", "HEAD~", "--", "out/d"],
            &["checkout", "HEAD~", "--", "."],
            &["checkout", "--no-overlay", "HEAD~", "--", "out"],
            &["checkout", "--ignore-skip-worktree-bits", "HEAD~", "--", "out"],
            &["checkout", "--ignore-skip-worktree-bits", "HEAD~", "--", "out/d"],
            &["checkout", "--ignore-skip-worktree-bits", "--no-overlay", "HEAD~", "--", "out"],
            &["checkout", "--ignore-skip-worktree-bits", "HEAD", "--", "."],
        ],
    );
}
