//! `git checkout --ignore-skip-worktree-bits -- <path>` writes a sparse-excluded
//! path out, but the index it stores still carries `CE_SKIP_WORKTREE` for it:
//! `checkout_worktree()` hands the entry to `checkout_entry()`, which never
//! touches `ce_flags` (builtin/checkout.c:447-450), and `write_locked_index()`
//! stores the bit (:701). The `H` a plain `ls-files -v` prints afterwards comes
//! from the reader's `clear_skip_worktree_from_present_files()`
//! (repository.c:467), in memory only.
//!
//! zvcs cleared the bit before writing, so its index differed on disk, and a
//! later stock `status` rewrote that index or left it depending on whether the
//! write landed in the checked-out file's mtime second. The raw bit is read
//! here with `sparse.expectFilesOutsideOfPatterns=true`, which turns the
//! read-time clear off (sparse-index.c:684-686), so the check does not depend
//! on timing.

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn git(bin: &str, dir: &Path, args: &[&str]) -> Run {
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
    Run {
        code: out.status.code().expect("no signal"),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// A cone-mode sparse checkout of `inside`, built by stock git, so the two
/// sides start from the same index.
fn fixture(stock: &str, label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-co-isw-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap().join("repo");
    for dir in ["inside", "outside/nested"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    git(stock, &root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("inside/keep.txt"), "kept\n").unwrap();
    std::fs::write(root.join("outside/drop.txt"), "excluded\n").unwrap();
    std::fs::write(root.join("outside/nested/deep.txt"), "excluded, nested\n").unwrap();
    std::fs::write(root.join("root.txt"), "root\n").unwrap();
    git(stock, &root, &["add", "-A"]);
    git(stock, &root, &["commit", "-q", "-m", "seed"]);
    git(stock, &root, &["sparse-checkout", "init", "--cone"]);
    git(stock, &root, &["sparse-checkout", "set", "inside"]);
    root
}

#[test]
fn ignore_skip_worktree_bits_keeps_the_bit_in_the_written_index() {
    let Some(stock) = stock_git() else { return };
    let args = ["checkout", "--ignore-skip-worktree-bits", "--", "outside/drop.txt"];
    let mut results = Vec::new();
    for (bin, label) in [(ZVCS, "zvcs"), (stock, "stock")] {
        let dir = fixture(stock, label);
        let run = git(bin, &dir, &args);
        let raw = git(stock, &dir, &["-c", "sparse.expectFilesOutsideOfPatterns=true", "ls-files", "-v"]);
        let cleared = git(stock, &dir, &["ls-files", "-v"]);
        let content = std::fs::read_to_string(dir.join("outside/drop.txt")).unwrap_or_default();
        results.push((run.code, run.stdout, run.stderr, raw.stdout, cleared.stdout, content));
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }
    assert_eq!(results[1].3.lines().find(|l| l.ends_with("outside/drop.txt")), Some("S outside/drop.txt"));
    assert_eq!(results[0], results[1], "left is zvcs, right is stock");
}
