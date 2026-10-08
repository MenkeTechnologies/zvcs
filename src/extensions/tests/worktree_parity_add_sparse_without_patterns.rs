//! `worktree add` with `core.sparseCheckout` on copies the main worktree's
//! `info/sparse-checkout` into the new administrative directory, and the checkout child's
//! `unpack_trees()` applies only patterns it can read. With no such file
//! `get_sparse_checkout_patterns()` fails, `skip_sparse_checkout` is set and the whole tree is
//! written out, with no `SKIP_WORKTREE` bit and an index that stays version 2. This port read
//! the missing file as an empty pattern list and excluded everything.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .output()
        .expect("run git");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

fn fixture(bin: &str, tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "zvcs-wt-add-sparse-{tag}-{}-{}",
        std::process::id(),
        if bin == BIN { "zvcs" } else { "stock" }
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("inside")).unwrap();
    std::fs::create_dir_all(dir.join("outside")).unwrap();
    std::fs::write(dir.join("inside/keep.txt"), "k\n").unwrap();
    std::fs::write(dir.join("outside/drop.txt"), "d\n").unwrap();
    std::fs::write(dir.join("root.txt"), "r\n").unwrap();
    run(bin, &dir, &["init", "-q", "-b", "main", "."]);
    run(bin, &dir, &["add", "-A"]);
    run(bin, &dir, &["commit", "-q", "-m", "one"]);
    dir
}

/// What the new worktree holds: its tracked-file tags, which files exist, and its index version.
fn worktree_state(bin: &str, dir: &Path) -> (String, Vec<String>, u32) {
    let wt = dir.join("wt");
    let tags = run(bin, &wt, &["ls-files", "-t"]).0;
    let mut files: Vec<String> = Vec::new();
    let mut stack = vec![wt.clone()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap().flatten() {
            let p = entry.path();
            if p.file_name().is_some_and(|n| n == ".git") {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
            } else {
                files.push(p.strip_prefix(&wt).unwrap().to_string_lossy().into_owned());
            }
        }
    }
    files.sort();
    let git_dir = std::fs::read_to_string(wt.join(".git")).unwrap();
    let admin = PathBuf::from(git_dir.trim().trim_start_matches("gitdir: "));
    let index = std::fs::read(admin.join("index")).unwrap();
    (tags, files, u32::from_be_bytes([index[4], index[5], index[6], index[7]]))
}

#[test]
fn sparse_enabled_without_a_pattern_file_checks_out_everything() {
    let Some(stock) = stock_git() else { return };
    let (s, z) = (fixture(stock, "nofile"), fixture(BIN, "nofile"));
    let args = ["-c", "core.sparseCheckout=1", "worktree", "add", "-q", "-b", "side", "wt"];
    assert_eq!(run(BIN, &z, &args), run(stock, &s, &args));
    let want = worktree_state(stock, &s);
    assert_eq!(worktree_state(BIN, &z), want);
    assert_eq!(want.1.len(), 3, "everything is written out: {want:?}");
    let _ = (std::fs::remove_dir_all(s), std::fs::remove_dir_all(z));
}

#[test]
fn an_existing_pattern_file_is_copied_and_applied() {
    let Some(stock) = stock_git() else { return };
    let (s, z) = (fixture(stock, "file"), fixture(BIN, "file"));
    for (bin, dir) in [(stock, &s), (BIN, &z)] {
        run(bin, dir, &["sparse-checkout", "set", "inside"]);
    }
    let args = ["worktree", "add", "-q", "-b", "side", "wt"];
    assert_eq!(run(BIN, &z, &args), run(stock, &s, &args));
    let want = worktree_state(stock, &s);
    assert_eq!(worktree_state(BIN, &z), want);
    assert!(want.0.contains("S outside/drop.txt"), "{want:?}");
    let _ = (std::fs::remove_dir_all(s), std::fs::remove_dir_all(z));
}
