//! The index a command writes over a sparse index, against the one stock git writes.
//!
//! git keeps a sparse index collapsed for the whole run of a sparse-aware command and writes
//! it back as it stands: `convert_to_sparse()` returns at once on an `INDEX_COLLAPSED` index
//! (sparse-index.c:207), so the cache-tree is not rebuilt either. A `git add` of an in-cone
//! path therefore leaves the `TREE` nodes above that path invalid, exactly as `add` left them.
//! This port expands every index it reads, and used to write the result back through a
//! from-scratch conversion that rebuilt the whole cache-tree; now an index git would be holding
//! collapsed is put back as it was read, and a full index is collapsed by a port of
//! `convert_to_sparse_rec()`.
//!
//! Each case builds one repository with stock git, copies it, runs the same command with stock
//! in one copy and with this binary in the other, and compares the two index files byte for
//! byte with only the filesystem facts masked: `ctime`, `mtime`, `dev`, `ino`, `uid` and
//! `gid` of every entry, and the trailing checksum that covers them.
//!
//! Skipped when no stock git is available: the sparse index is stock's to write.
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
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("GIT_ADVICE", "0")
        .output()
        .unwrap()
}

fn ok(bin: &str, dir: &Path, home: &Path, args: &[&str]) {
    let out = git(bin, dir, home, args);
    assert!(out.status.success(), "`{bin} {args:?}` failed: {out:?}");
}

/// Every worktree file of `dir` back to one fixed, old mtime, so no entry is racy and both
/// copies see the same stat data.
fn age(dir: &Path) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.file_name().is_some_and(|n| n == ".git") {
            continue;
        }
        if path.is_dir() {
            age(&path);
        } else {
            let t = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_577_836_800);
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(t)
                .unwrap();
        }
    }
}

fn copy_dir(from: &Path, to: &Path) {
    let status = Command::new("cp").arg("-Rp").arg(from).arg(to).status().unwrap();
    assert!(status.success());
}

/// The index file with the filesystem facts of each entry zeroed and the checksum dropped.
fn masked_index(dir: &Path) -> Vec<u8> {
    let mut b = std::fs::read(dir.join(".git/index")).unwrap();
    assert_eq!(&b[..4], b"DIRC");
    let version = u32::from_be_bytes(b[4..8].try_into().unwrap());
    assert!(version == 2 || version == 3, "v{version} is not walked here");
    let n = u32::from_be_bytes(b[8..12].try_into().unwrap());
    let mut pos = 12;
    for _ in 0..n {
        // ctime, mtime, dev, ino (24); uid, gid (8). A file the command writes is dated by
        // the second it ran in.
        b[pos..pos + 24].fill(0);
        b[pos + 28..pos + 36].fill(0);
        let flags = u16::from_be_bytes([b[pos + 60], b[pos + 61]]);
        let name_at = pos + 62 + if flags & 0x4000 != 0 { 2 } else { 0 };
        let nul = b[name_at..].iter().position(|c| *c == 0).unwrap();
        pos += (name_at + nul + 1 - pos).div_ceil(8) * 8;
    }
    b.truncate(b.len() - 20);
    b
}

impl Fixture {
    /// `top`, `in/x`, `in/sub/y`, `out1/p`, `out1/deep/q`, `out2/r` over two commits, with
    /// `other` at the first; then cone `in` under `index.sparse=true`, so stock's index holds
    /// `out1/` and `out2/` as sparse-directory entries.
    fn new(stock: &str, tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-sparse-write-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let base = root.join("base");
        std::fs::create_dir_all(&base).unwrap();
        let f = Fixture { root };
        let write = |p: &str, body: &str| {
            let full = base.join(p);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, body).unwrap();
        };
        ok(stock, &base, &f.root, &["init", "-q", "-b", "main", "."]);
        for p in ["top", "in/x", "in/sub/y", "out1/p", "out1/deep/q", "out2/r"] {
            write(p, &format!("{p}\n"));
        }
        ok(stock, &base, &f.root, &["add", "."]);
        ok(stock, &base, &f.root, &["commit", "-q", "-m", "one"]);
        write("in/x", "two\n");
        write("out1/p", "two\n");
        ok(stock, &base, &f.root, &["commit", "-q", "-a", "-m", "two"]);
        ok(stock, &base, &f.root, &["branch", "other", "HEAD~1"]);
        ok(stock, &base, &f.root, &["sparse-checkout", "set", "--cone", "--sparse-index", "in"]);
        age(&base);
        ok(stock, &base, &f.root, &["update-index", "--refresh"]);
        assert!(
            std::fs::read(base.join(".git/index")).unwrap().windows(4).any(|w| w == b"sdir"),
            "stock must have written a sparse index for this test to mean anything"
        );
        copy_dir(&base, &f.root.join("stock"));
        copy_dir(&base, &f.root.join("ours"));
        f
    }

    /// Run `prep` and then `args` in both copies — `args` with stock in one and this binary in
    /// the other — and return the two masked index files.
    fn both(&self, stock: &str, prep: &dyn Fn(&Path), args: &[&str]) -> (Vec<u8>, Vec<u8>) {
        let (s, o) = (self.root.join("stock"), self.root.join("ours"));
        prep(&s);
        prep(&o);
        ok(stock, &s, &self.root, args);
        ok(BIN, &o, &self.root, args);
        (masked_index(&s), masked_index(&o))
    }
}

/// Rewrite `in/x` with an old mtime, as an edit the test controls the stat data of.
fn edit_in_x(dir: &Path) {
    let path = dir.join("in/x");
    std::fs::write(&path, "edited\n").unwrap();
    let t = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_577_840_400);
    std::fs::File::options().write(true).open(&path).unwrap().set_modified(t).unwrap();
}

#[test]
fn add_inside_the_cone_keeps_the_index_collapsed_with_the_cache_tree_add_left() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, "add");
    let (theirs, ours) = f.both(stock, &edit_in_x, &["add", "in/x"]);
    assert!(ours.windows(4).any(|w| w == b"sdir"), "the index stays sparse");
    assert_eq!(ours, theirs);
}

#[test]
fn rm_cached_inside_the_cone_keeps_the_index_collapsed() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, "rm");
    let (theirs, ours) = f.both(stock, &|_| {}, &["rm", "-q", "--cached", "in/x"]);
    assert_eq!(ours, theirs);
}

#[test]
fn commit_writes_the_collapsed_index_with_its_cache_tree() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, "commit");
    let (theirs, ours) = f.both(stock, &edit_in_x, &["commit", "-q", "-a", "-m", "three"]);
    assert_eq!(ours, theirs);
}

#[test]
fn a_command_that_needs_a_full_index_collapses_it_again_from_scratch() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, "mv");
    let (theirs, ours) = f.both(stock, &|_| {}, &["mv", "in/x", "in/x2"]);
    assert!(ours.windows(4).any(|w| w == b"sdir"), "the index is collapsed again on write");
    assert_eq!(ours, theirs);
}

#[test]
fn reset_of_a_path_outside_the_cone_expands_and_collapses_again() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, "reset");
    let (theirs, ours) = f.both(stock, &|_| {}, &["reset", "-q", "HEAD~1", "--", "out1/p"]);
    assert_eq!(ours, theirs);
}

#[test]
fn a_mixed_reset_keeps_the_index_collapsed() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, "reset-mixed");
    let (theirs, ours) = f.both(stock, &|_| {}, &["reset", "-q", "HEAD~1"]);
    assert_eq!(ours, theirs);
}

#[test]
fn a_branch_switch_carries_a_moved_sparse_directory_across_collapsed() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, "checkout");
    let (theirs, ours) = f.both(stock, &|_| {}, &["checkout", "-q", "other"]);
    assert_eq!(ours, theirs);
    assert!(!f.root.join("ours/out1").exists(), "the switch writes nothing outside the cone");
}
