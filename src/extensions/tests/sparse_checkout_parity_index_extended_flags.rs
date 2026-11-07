//! The index a command writes over skip-worktree entries, against the one stock git writes.
//!
//! Three things decide those bytes, and each one diverged:
//!
//! * **The extended flags word.** `do_write_index()` recomputes `CE_EXTENDED` from
//!   `CE_EXTENDED_FLAGS` for every entry before it writes one, and demotes version 3 to 2 when
//!   no entry needs the word (read-cache.c:2847-2872). An entry read with the word keeps the
//!   in-memory bit after `clear_skip_worktree_from_present_files()` (sparse-index.c:650-691)
//!   drops its skip-worktree flag, so a writer that trusted the bit wrote a version 3 index
//!   whose entries carried an all-zero extended word where stock writes version 2.
//! * **The cache-tree `sparse-checkout` writes.** `update_sparsity()` (unpack-trees.c:2132-2196)
//!   never calls `cache_tree_update()`, so nodes an earlier `update-index` invalidated stay
//!   invalid; rebuilding them wrote a longer `TREE` extension.
//! * **The stat data of the files `sparse-checkout add` materializes.** `check_updates()` writes
//!   them before the index is written (unpack-trees.c:2185, builtin/sparse-checkout.c:246-247)
//!   and records their stat data (entry.c:269-279); writing the index first left an expanded
//!   sparse directory's entries with a zero size.
//!
//! Each case builds one repository with stock git, copies it, runs the same command with stock
//! in one copy and with this binary in the other, and compares the two index files byte for
//! byte with only the filesystem facts masked: `ctime`, `mtime`, `dev`, `ino`, `uid` and `gid`
//! of every entry, and the trailing checksum that covers them.
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
        .env("GIT_ADVICE", "0")
        .output()
        .unwrap()
}

fn ok(bin: &str, dir: &Path, home: &Path, args: &[&str]) {
    let out = git(bin, dir, home, args);
    assert!(out.status.success(), "`{bin} {args:?}` failed: {out:?}");
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
        // ctime, mtime, dev, ino (24); uid, gid (8).
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

fn header_version(dir: &Path) -> u32 {
    let b = std::fs::read(dir.join(".git/index")).unwrap();
    u32::from_be_bytes(b[4..8].try_into().unwrap())
}

impl Fixture {
    /// `root.txt`, `inside/keep.txt`, `inside/nested/also.txt`, `outside/drop.txt` and
    /// `outside/nested/deep.txt` over two commits (the second changes `root.txt`), then cone
    /// `inside`, with `extra` run by stock git after it.
    fn new(stock: &str, tag: &str, sparse_index: bool) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-sparse-extended-{tag}-{}", std::process::id()));
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
        for p in ["root.txt", "inside/keep.txt", "inside/nested/also.txt", "outside/drop.txt", "outside/nested/deep.txt"] {
            write(p, &format!("{p}\n"));
        }
        ok(stock, &base, &f.root, &["add", "."]);
        ok(stock, &base, &f.root, &["commit", "-q", "-m", "one"]);
        write("root.txt", "two\n");
        ok(stock, &base, &f.root, &["commit", "-q", "-a", "-m", "two"]);
        let mut set = vec!["sparse-checkout", "set", "--cone"];
        if sparse_index {
            set.push("--sparse-index");
        }
        set.push("inside");
        ok(stock, &base, &f.root, &set);
        f
    }

    fn base(&self) -> PathBuf {
        self.root.join("base")
    }

    /// Run `prep` with stock git in the base, then `args` with each binary in its own copy,
    /// and return the two masked indexes, stock's first.
    fn compare(&self, stock: &str, prep: &[&[&str]], args: &[&str]) -> ((Vec<u8>, u32), (Vec<u8>, u32)) {
        for step in prep {
            ok(stock, &self.base(), &self.root, step);
        }
        let sides = [("stock", stock), ("zvcs", BIN)].map(|(name, bin)| {
            let dir = self.root.join(name);
            copy_dir(&self.base(), &dir);
            ok(bin, &dir, &self.root, args);
            (masked_index(&dir), header_version(&dir))
        });
        let [s, z] = sides;
        (s, z)
    }
}

/// `checkout --ignore-skip-worktree-bits` writes the excluded files and keeps their
/// `CE_SKIP_WORKTREE` (builtin/checkout.c:447-450, :701), so the next writer reads them back
/// present, drops the bit, and must write version 2 with no extended word.
#[test]
fn a_rewrite_after_ignore_skip_worktree_bits_drops_the_extended_word() {
    let Some(stock) = stock_git() else { return };
    for (tag, args) in [
        ("refresh", &["update-index", "--really-refresh"][..]),
        ("version2", &["update-index", "--index-version", "2"][..]),
    ] {
        let f = Fixture::new(stock, tag, false);
        let prep: &[&[&str]] = &[&["checkout", "-q", "--overlay", "--ignore-skip-worktree-bits", "HEAD", "--", "."]];
        let ((s, sv), (z, zv)) = f.compare(stock, prep, args);
        assert_eq!(sv, 2, "stock demotes to version 2 once no entry needs the word");
        assert_eq!(zv, sv, "`{args:?}`: index version");
        assert!(s == z, "`{args:?}`: index bytes differ from stock's");
    }
}

/// The checkout itself leaves the bit on disk, so its own index is version 3, with the word.
#[test]
fn checkout_ignore_skip_worktree_bits_keeps_the_bit_on_disk() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, "co", false);
    let ((s, sv), (z, zv)) =
        f.compare(stock, &[], &["checkout", "-q", "--overlay", "--ignore-skip-worktree-bits", "HEAD", "--", "."]);
    assert_eq!(sv, 3);
    assert_eq!(zv, sv);
    assert!(s == z, "index bytes differ from stock's");
}

/// `reapply` over a cache-tree `update-index` invalidated writes it back invalidated.
#[test]
fn reapply_keeps_the_cache_tree_update_index_invalidated() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, "reapply", false);
    let prep: &[&[&str]] = &[
        &["update-index", "--no-skip-worktree", "outside/drop.txt"],
        &["update-index", "--skip-worktree", "root.txt"],
    ];
    let ((s, _), (z, _)) = f.compare(stock, prep, &["sparse-checkout", "reapply"]);
    assert!(s == z, "index bytes differ from stock's");
}

/// `sparse-checkout add` over a sparse index records the files it materializes.
#[test]
fn add_over_a_sparse_index_records_the_materialized_files() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, "add", true);
    let ((s, _), (z, _)) = f.compare(stock, &[], &["sparse-checkout", "add", "outside"]);
    assert!(s == z, "index bytes differ from stock's");
}
