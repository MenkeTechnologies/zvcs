//! `git checkout [<tree-ish>] -- <paths>` and `git restore` over a sparse index.
//!
//! `checkout_main()` clears `command_requires_full_index` (builtin/checkout.c:1904), so a
//! directory the cone leaves out is one `040000` entry for the whole of `checkout_paths()`:
//!
//! * `read_tree_some()` hands each source tree to `try_update_sparse_directory()`
//!   (builtin/checkout.c:149-190): the same tree is skipped, a different one replaces the
//!   entry's id in no-overlay mode and is descended into in overlay mode, where each blob
//!   goes through `add_index_entry()` — appended after the sparse directory when it sorts
//!   last (read-cache.c:1262-1265), otherwise expanding the index (read-cache.c:543-560).
//! * the marking pass sees the directory as one skip-worktree entry, so `out/b` is
//!   `did not match`, while `--ignore-skip-worktree-bits out` matches the directory itself
//!   and `checkout_entry()` refuses it with `unknown file mode for out/ in index`.
//!
//! zvcs matched the files inside the directory one by one, wrote them out and expanded the
//! index on write, which the next stock reader reported with the expansion advice.
//!
//! The written index is compared entry by entry (stat data left out) together with its
//! cache-tree, since `try_update_sparse_directory()` moves an entry without invalidating it.

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git_at_least;

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
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The shapes the index can have after `sparse-checkout reapply --sparse-index` of cone `in`.
#[derive(Clone, Copy, Debug)]
struct Shape {
    /// A tracked `top` sorts after the last sparse directory, so `add_index_entry()` searches
    /// instead of appending.
    top: bool,
    /// `sparse.expectFilesOutsideOfPatterns`, which keeps a directory created in the work tree
    /// from expanding the next reader's index.
    expect_outside: bool,
    /// An empty `out/` made in the work tree before the command runs: the read's
    /// `clear_skip_worktree_from_present_files()` then expands the index with the advice
    /// (sparse-index.c:689-692) unless `expect_outside`, and otherwise a matched `out/`
    /// `lstat()`s, so `checkout_entry()` aborts there and nothing sorting after it is written.
    present: bool,
}

/// Two commits that change `in/f`, `out/b`, `out/d` and `deep/x/y/z` (and `top`), then a cone
/// sparse checkout of `in` with a sparse index, built by stock git so both sides start from
/// the same collapsed index: `deep/` and `out/` are sparse directories.
fn fixture(stock: &str, tag: &str, shape: Shape) -> PathBuf {
    let base = std::env::temp_dir().join(format!("zvcs-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let root = std::fs::canonicalize(&base).unwrap().join("repo");
    for dir in ["in", "out/sub", "deep/x/y"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    git(stock, &root, &["init", "-q", "-b", "main"]);
    let mut one = vec![("a", "a\n"), ("in/f", "in1\n"), ("out/b", "b1\n"), ("out/d", "d1\n"), ("out/sub/s", "s1\n"), ("deep/x/y/z", "y1\n")];
    let mut two = vec![("in/f", "in2\n"), ("out/b", "b2\n"), ("out/d", "d2\n"), ("deep/x/y/z", "y2\n")];
    if shape.top {
        one.push(("top", "t1\n"));
        two.push(("top", "t2\n"));
    }
    for (p, c) in one {
        std::fs::write(root.join(p), c).unwrap();
    }
    git(stock, &root, &["add", "-A"]);
    git(stock, &root, &["commit", "-q", "-m", "one"]);
    for (p, c) in two {
        std::fs::write(root.join(p), c).unwrap();
    }
    git(stock, &root, &["commit", "-q", "-a", "-m", "two"]);
    git(stock, &root, &["sparse-checkout", "set", "--cone", "in"]);
    git(stock, &root, &["sparse-checkout", "reapply", "--sparse-index"]);
    if shape.expect_outside {
        git(stock, &root, &["config", "sparse.expectFilesOutsideOfPatterns", "true"]);
    }
    if shape.present {
        std::fs::create_dir(root.join("out")).unwrap();
    }
    root
}

/// The index file without stat data: each entry's mode, id, flags and name, then every
/// extension, the cache-tree decoded.
fn dump_index(path: &Path) -> String {
    let Ok(d) = std::fs::read(path) else { return "<no index>".into() };
    let be32 = |o: usize| u32::from_be_bytes(d[o..o + 4].try_into().unwrap());
    let be16 = |o: usize| u16::from_be_bytes(d[o..o + 2].try_into().unwrap());
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    let (version, n) = (be32(4), be32(8));
    let mut out = format!("v{version} n={n}\n");
    let mut o = 12;
    for _ in 0..n {
        let mode = be32(o + 24);
        let id = &d[o + 40..o + 60];
        let flags = be16(o + 60);
        let mut p = o + 62;
        let mut extended = 0;
        if flags & 0x4000 != 0 {
            extended = be16(p);
            p += 2;
        }
        let end = p + d[p..].iter().position(|b| *b == 0).unwrap();
        let name = String::from_utf8_lossy(&d[p..end]);
        out += &format!("{mode:o} {} {:04x} {extended:04x} {name}\n", hex(id), flags & 0xf000);
        o += (end - o + 1 + 7) & !7;
    }
    while o + 8 <= d.len() - 20 {
        let sig = String::from_utf8_lossy(&d[o..o + 4]).into_owned();
        let len = be32(o + 4) as usize;
        let body = &d[o + 8..o + 8 + len];
        o += 8 + len;
        if sig != "TREE" {
            out += &format!("{sig} {len}\n");
            continue;
        }
        let mut q = 0;
        while q < body.len() {
            let z = q + body[q..].iter().position(|b| *b == 0).unwrap();
            let name = String::from_utf8_lossy(&body[q..z]).into_owned();
            let nl = z + 1 + body[z + 1..].iter().position(|b| *b == b'\n').unwrap();
            let counts = String::from_utf8_lossy(&body[z + 1..nl]).into_owned();
            q = nl + 1;
            let mut id = String::new();
            if !counts.starts_with('-') {
                id = hex(&body[q..q + 20]);
                q += 20;
            }
            out += &format!("TREE '{name}' {counts} {id}\n");
        }
    }
    out
}

/// Exit code, output, the written index, a leftover lock, then what the next stock reader
/// makes of it (the expansion advice included) and the work tree.
fn observe(bin: &str, stock: &str, tag: &str, shape: Shape, args: &[&str]) -> Vec<String> {
    let dir = fixture(stock, tag, shape);
    let (code, out, err) = git(bin, &dir, args);
    let mut seen = vec![code.to_string(), out, err, dump_index(&dir.join(".git/index"))];
    seen.push(dir.join(".git/index.lock").exists().to_string());
    for probe in [&["ls-files", "--sparse", "-s"][..], &["status", "--porcelain"]] {
        let (_, o, e) = git(stock, &dir, probe);
        seen.push(o + &e);
    }
    for p in ["out", "out/b", "out/d", "out/sub/s", "deep", "deep/x/y/z"] {
        let at = dir.join(p);
        seen.push(match std::fs::read_to_string(&at) {
            Ok(c) => format!("{p}: {c}"),
            Err(_) if at.is_dir() => format!("{p}: <dir>"),
            Err(_) => format!("{p}: <absent>"),
        });
    }
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    seen
}

const SHAPES: [Shape; 4] = [
    Shape { top: false, expect_outside: false, present: false },
    Shape { top: true, expect_outside: false, present: false },
    Shape { top: false, expect_outside: true, present: false },
    Shape { top: true, expect_outside: true, present: false },
];

fn compare(tag: &str, cases: &[&[&str]]) {
    compare_in(tag, &SHAPES, cases);
}

fn compare_in(tag: &str, shapes: &[Shape], cases: &[&[&str]]) {
    let Some(stock) = stock_git_at_least((2, 56, 0)) else { return };
    for (k, &shape) in shapes.iter().enumerate() {
        for (n, args) in cases.iter().enumerate() {
            let z = observe(ZVCS, stock, &format!("{tag}-z{n}-{k}"), shape, args);
            let s = observe(stock, stock, &format!("{tag}-s{n}-{k}"), shape, args);
            assert_eq!(z, s, "{shape:?} {args:?}: left is zvcs, right is stock");
        }
    }
}

/// Without a source the index is the source: only `--ignore-skip-worktree-bits` reaches a
/// sparse directory, and then only as a whole.
#[test]
fn from_the_index_a_sparse_directory_matches_only_as_itself() {
    compare(
        "co-si-index",
        &[
            &["restore", "--ignore-skip-worktree-bits", "out/b"],
            &["restore", "--ignore-skip-worktree-bits", "out"],
            &["restore", "--ignore-skip-worktree-bits", "."],
            &["restore", "out/b"],
            &["checkout", "--ignore-skip-worktree-bits", "--", "out/b"],
            &["checkout", "--ignore-skip-worktree-bits", "--", "out"],
        ],
    );
}

/// `try_update_sparse_directory()` in no-overlay mode: a different tree replaces the
/// directory's id without descending and without invalidating its cache-tree node.
#[test]
fn no_overlay_moves_the_sparse_directory_to_the_source_tree() {
    compare(
        "co-si-nooverlay",
        &[
            &["restore", "--source=HEAD~", "--staged", "out/b"],
            &["restore", "--source=HEAD~", "--staged", "."],
            &["restore", "--source=HEAD~", "--staged", "*/z", "a"],
            &["restore", "--source=HEAD~", "--staged", ".", ":!out"],
            &["restore", "--source=HEAD~", "--staged", "--worktree", "--ignore-skip-worktree-bits", "."],
            &["restore", "--source=HEAD", "--staged", "--ignore-skip-worktree-bits", "out"],
            &["checkout", "--no-overlay", "HEAD~", "--", "out"],
            &["checkout", "--no-overlay", "HEAD", "--ignore-skip-worktree-bits", "--", "."],
        ],
    );
}

/// `try_update_sparse_directory()` in overlay mode descends, and `add_index_entry()` either
/// appends next to the sparse directory or expands the index; the blob that expanded it is a
/// fresh entry even where the tree agrees with the one the expansion made.
#[test]
fn overlay_descends_and_appends_or_expands() {
    compare(
        "co-si-overlay",
        &[
            &["checkout", "HEAD~", "--", "out/d"],
            &["checkout", "HEAD~", "--", "out"],
            &["checkout", "HEAD~", "--", "out/sub/s"],
            &["checkout", "HEAD~", "--", "out/d", "out/sub/s"],
            &["checkout", "HEAD~", "out/d"],
            &["checkout", "HEAD~", "--", "deep/x"],
            &["restore", "--source=HEAD~", "--overlay", "--staged", "--worktree", "out"],
        ],
    );
}

/// A sparse directory standing in the work tree: without `sparse.expectFilesOutsideOfPatterns`
/// the read expands the index and gives the advice on the command's own stderr; with it the
/// index stays collapsed, a matched `out/` `lstat()`s and `checkout_entry()` aborts with the
/// index still locked, after writing only what sorts before `out/` (so `top` is not written).
#[test]
fn a_present_sparse_directory_expands_on_read_or_aborts_the_walk() {
    compare_in(
        "co-si-present",
        &[
            Shape { top: true, expect_outside: false, present: true },
            Shape { top: true, expect_outside: true, present: true },
        ],
        &[
            &["restore", "--ignore-skip-worktree-bits", "out"],
            &["restore", "--source=HEAD~", "--staged", "--worktree", "--ignore-skip-worktree-bits", "."],
            &["checkout", "--ignore-skip-worktree-bits", "--", "."],
            &["checkout", "HEAD~", "--", "out/d"],
        ],
    );
}
