//! A worktree restore under `GIT_ATTR_SOURCE=` (names no tree-ish).
//!
//! `checkout_entry()` returns before any attribute lookup for a file `ie_match_stat()`
//! calls unchanged, and `ie_match_stat()` reads a file only when its recorded `mtime` is
//! not older than the index's. So stock dies with `bad --attr-source or GIT_ATTR_SOURCE`
//! (128) when a regular file is written or compared — a modified file, a missing
//! one, a freshly written (racy) one — and says nothing for a settled clean tree or a
//! symlink's write. A racy symlink is compared, and compared like a file. zvcs never consulted
//! the attribute source on this path and restored silently. A file that is rewritten is
//! unlinked first, so the die leaves it gone.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(bin: &str, dir: &Path, args: &[&str]) -> (String, String, Option<i32>) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("HOME", dir)
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
    )
}

fn attr_src(bin: &str, dir: &Path, args: &[&str]) -> (String, String, Option<i32>) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_ATTR_SOURCE", "")
        .env("HOME", dir)
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
    )
}

/// Committed `README.md`, `src/lib.rs` and a symlink `lnk`, built with `bin`.
fn fixture(tag: &str, bin: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-restoreattr-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let dir = dir.canonicalize().unwrap();
    std::fs::write(dir.join("README.md"), "a\n").unwrap();
    std::fs::write(dir.join("src/lib.rs"), "b\n").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("README.md", dir.join("lnk")).unwrap();
    git(bin, &dir, &["init", "-q", "-b", "main"]);
    git(bin, &dir, &["add", "."]);
    assert_eq!(git(bin, &dir, &["commit", "-qm", "one"]).2, Some(0));
    dir
}

const RESTORES: [&[&str]; 5] = [
    &["restore", "README.md"],
    &["restore", "."],
    &["restore", "--source=HEAD", "."],
    &["restore", "lnk"],
    &["checkout", "HEAD", "--", "."],
];

/// Every restore form on a fresh fixture, built and shaped by `stock` so both runners see the
/// same index, then run with `bin`.
/// Name and content of every file and symlink in the work tree outside `.git`.
fn worktree_snapshot(dir: &Path) -> Vec<String> {
    let mut seen = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap().flatten() {
            let path = entry.path();
            let rel = path.strip_prefix(dir).unwrap().to_string_lossy().into_owned();
            if rel == ".git" {
                continue;
            }
            let ft = entry.file_type().unwrap();
            if ft.is_dir() {
                stack.push(path);
            } else if ft.is_symlink() {
                seen.push(format!("{rel} -> {}", std::fs::read_link(&path).unwrap().display()));
            } else {
                seen.push(format!("{rel}: {:?}", std::fs::read_to_string(&path).unwrap_or_default()));
            }
        }
    }
    seen.sort();
    seen
}

type Outcome = ((String, String, Option<i32>), Vec<String>);

fn outcomes(stock: &str, bin: &str, tag: &str, prepare: impl Fn(&str, &Path)) -> Vec<Outcome> {
    RESTORES
        .iter()
        .map(|args| {
            let dir = fixture(tag, stock);
            prepare(stock, &dir);
            let result = (attr_src(bin, &dir, args), worktree_snapshot(&dir));
            let _ = std::fs::remove_dir_all(&dir);
            result
        })
        .collect()
}

/// Stamp the index file's mtime: git's racy test compares an entry's recorded `mtime` with it,
/// so pinning it makes "racy" and "settled" independent of the wall clock.
fn stamp_index(dir: &Path, when: std::time::SystemTime) {
    let file = std::fs::OpenOptions::new().write(true).open(dir.join(".git/index")).unwrap();
    file.set_modified(when).unwrap();
}

fn long_ago() -> std::time::SystemTime {
    std::time::UNIX_EPOCH + std::time::Duration::from_secs(86_400)
}

fn far_ahead() -> std::time::SystemTime {
    std::time::SystemTime::now() + std::time::Duration::from_secs(3_600)
}

#[test]
fn a_restore_that_writes_or_compares_a_regular_file_dies_on_a_bad_attr_source() {
    let Some(stock) = stock_git() else { return };
    // The index is older than every entry: all of them are racy and get compared.
    let racy = |_: &str, dir: &Path| stamp_index(dir, long_ago());
    // The index is newer than every entry: only a real difference reaches attributes.
    let modified = |_: &str, dir: &Path| {
        stamp_index(dir, far_ahead());
        std::fs::write(dir.join("README.md"), "a\nmore\n").unwrap();
    };
    let missing = |_: &str, dir: &Path| {
        stamp_index(dir, far_ahead());
        std::fs::remove_file(dir.join("README.md")).unwrap();
    };
    let link_retargeted = |_: &str, dir: &Path| {
        stamp_index(dir, far_ahead());
        std::fs::remove_file(dir.join("lnk")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("src/lib.rs", dir.join("lnk")).unwrap();
    };
    assert_eq!(outcomes(stock, BIN, "racy", racy), outcomes(stock, stock, "racy", racy));
    assert_eq!(outcomes(stock, BIN, "mod", modified), outcomes(stock, stock, "mod", modified));
    assert_eq!(outcomes(stock, BIN, "del", missing), outcomes(stock, stock, "del", missing));
    assert_eq!(outcomes(stock, BIN, "lnk", link_retargeted), outcomes(stock, stock, "lnk", link_retargeted));
}

#[test]
fn a_settled_clean_tree_restores_without_consulting_the_attr_source() {
    let Some(stock) = stock_git() else { return };
    let settled = |_: &str, dir: &Path| stamp_index(dir, far_ahead());
    assert_eq!(outcomes(stock, BIN, "settled", settled), outcomes(stock, stock, "settled", settled));
}

