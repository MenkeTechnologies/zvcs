//! `git sparse-checkout init --sparse-index` on a repository whose index has no
//! skip-worktree entry yet.
//!
//! Reading an index with `index.sparse` on marks it `INDEX_COLLAPSED` even though
//! no directory has collapsed (`ensure_correct_sparsity()`, sparse-index.c:475-484).
//! `update_sparsity()` then calls `expand_index()` (unpack-trees.c:2157-2158), which
//! returns the index to `INDEX_EXPANDED`, so the write that follows runs
//! `convert_to_sparse()` and stores each directory outside the cone as one
//! `040000` sparse-directory entry. zvcs only expanded an index that already held a
//! sparse-directory entry, so the flag stayed set, the write skipped the
//! conversion, and the result was a full index carrying an `sdir` extension.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Root(PathBuf);

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn git(bin: &str, home: &Path, work: &Path, args: &[&str]) -> String {
    let out = Command::new(bin)
        .args(args)
        .current_dir(work)
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1112911993 +0000")
        .env("GIT_COMMITTER_DATE", "1112911993 +0000")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .unwrap();
    assert!(out.status.success(), "`{bin} {args:?}` failed: {out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A committed repository with two root files and directories that nest, built and
/// sparsified by `bin`; returns what stock git lists of the finished index.
fn sparse_listing(bin: &str, tag: &str, init_args: &[&str]) -> (String, Vec<u8>, Root) {
    let root = Root(std::env::temp_dir().join(format!("zvcs-sc-init-si-{tag}-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&root.0);
    let work = root.0.join("work");
    for d in ["d/e", "inside"] {
        std::fs::create_dir_all(work.join(d)).unwrap();
    }
    for p in ["a", "README.md", "d/b", "d/e/c", "inside/keep.txt"] {
        std::fs::write(work.join(p), format!("{p}\n")).unwrap();
    }
    git(bin, &root.0, &work, &["init", "-q", "-b", "main", "."]);
    git(bin, &root.0, &work, &["add", "-A"]);
    git(bin, &root.0, &work, &["commit", "-q", "-m", "initial"]);

    let mut args = vec!["sparse-checkout", "init"];
    args.extend_from_slice(init_args);
    git(bin, &root.0, &work, &args);

    let stock = stock_git().expect("caller checked");
    let listing = git(stock, &root.0, &work, &["ls-files", "--sparse", "--stage"]);
    let index = std::fs::read(work.join(".git/index")).unwrap();
    (listing, index, root)
}

fn assert_collapsed_like_stock(init_args: &[&str], tag: &str) {
    let Some(stock) = stock_git() else { return };
    let (want, want_index, _a) = sparse_listing(stock, &format!("{tag}-stock"), init_args);
    let (got, got_index, _b) = sparse_listing(BIN, &format!("{tag}-zvcs"), init_args);

    assert!(
        want.contains("040000") && want.contains("\td/\n"),
        "oracle did not collapse `d/`; the test premise is gone:\n{want}"
    );
    assert_eq!(got, want, "stock git reads zvcs's sparse index differently");
    assert_eq!(
        got_index.len(),
        want_index.len(),
        "index size differs: zvcs wrote a structure stock git would not have"
    );
}

#[test]
fn init_cone_sparse_index_collapses_directories_outside_the_cone() {
    assert_collapsed_like_stock(&["--cone", "--sparse-index"], "cone");
}

#[test]
fn init_sparse_index_without_cone_flag_collapses_directories_outside_the_cone() {
    assert_collapsed_like_stock(&["--sparse-index"], "plain");
}
