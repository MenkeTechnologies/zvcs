//! `status` and `diff --cached` over a sparse index stock git wrote.
//!
//! With `index.sparse=true` stock writes the `sdir` extension, and every directory wholly
//! outside the cone as one `040000` entry. Both commands compare `HEAD`'s tree with the
//! index; the vendored tree-to-index diff refuses a sparse index outright, so both died with
//! `Cannot diff indices that contain sparse entries` — on a cone that excluded nothing just
//! as on one that did, because the extension alone marks the index sparse.
//!
//! git's `diff-index` over a sparse-directory entry reports what comparing the tree it names
//! would, so the comparison runs over the index `ensure_full_index()` (sparse-index.c:469-474)
//! expands it to, leaving the index itself as it was read.
//!
//! Skipped when no stock git is available: the sparse index is stock's to write.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// `top`, `a/x`, `c/z` committed; cone `a` under `index.sparse=true`, so stock's index
    /// holds `c/` as one sparse-directory entry; then `a/x` changed and staged.
    fn new(stock: &str, tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-status-sparse-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(stock, &["init", "-q", "-b", "main", "."]);
        for p in ["top", "a/x", "c/z"] {
            let full = f.work.join(p);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, format!("{p}\n")).unwrap();
        }
        f.git(stock, &["add", "."]);
        f.git(stock, &["commit", "-q", "-m", "one"]);
        f.git(stock, &["sparse-checkout", "set", "--cone", "--sparse-index", "a"]);
        std::fs::write(f.work.join("a/x"), "changed\n").unwrap();
        f.git(stock, &["add", "a/x"]);
        let index = std::fs::read(f.work.join(".git/index")).unwrap();
        assert!(
            index.windows(4).any(|w| w == b"sdir"),
            "stock must have written a sparse index for this test to mean anything"
        );
        f
    }

    fn git(&self, bin: &str, args: &[&str]) -> String {
        let out = Command::new(bin)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@example.com")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@example.com")
            .env("GIT_ADVICE", "0")
            .output()
            .unwrap();
        assert!(out.status.success(), "`{bin} {args:?}` failed: {out:?}");
        String::from_utf8(out.stdout).unwrap()
    }
}

#[test]
fn status_reads_a_sparse_index_as_the_full_index_it_stands_for() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, "status");
    let ours = f.git(BIN, &["status", "--porcelain"]);
    assert_eq!(ours, "M  a/x\n");
    assert_eq!(ours, f.git(stock, &["status", "--porcelain"]));
}

#[test]
fn diff_cached_reads_a_sparse_index_as_the_full_index_it_stands_for() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, "diff");
    let ours = f.git(BIN, &["diff", "--cached", "--name-status"]);
    assert_eq!(ours, "M\ta/x\n");
    assert_eq!(ours, f.git(stock, &["diff", "--cached", "--name-status"]));
}
