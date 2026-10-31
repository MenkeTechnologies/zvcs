//! `git diff <rev>` over a path the index moved off the tree and the worktree moved
//! off the index.
//!
//! `show_modified()` takes the post-image from `get_stat_data()` of the index entry
//! (diff-lib.c:315-347, :399, v2.56.0): a file whose stat no longer matches its entry
//! is the worktree side, read from the file, however the index differs from the
//! tree. The port runs the tree↔index and index↔worktree passes on two threads
//! whose items arrive in either order, and a tree↔index item that came in second
//! replaced the worktree side with the index id. A large tree ahead of the path
//! keeps the tree↔index thread busy long after the worktree thread reported it,
//! which made that order the usual one rather than a rare one on a loaded runner.
#![cfg(unix)]

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
        .env("GIT_AUTHOR_DATE", "2005-04-07T22:13:13 +0000")
        .env("GIT_COMMITTER_DATE", "2005-04-07T22:13:13 +0000")
        .env("LC_ALL", "C")
        .output()
        .expect("run git");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

/// `b` is committed as `b`, staged as `b2` and edited to `b3` with an mtime that
/// cannot match its entry, behind 3000 committed directories `z0000/f`..`z2999/f`.
fn fixture(stock: &str, tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-tree-worktree-order-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let git = |args: &[&str]| run(stock, &dir, args);
    git(&["init", "-q", "-b", "main", "."]);
    for i in 0..3000 {
        let sub = dir.join(format!("z{i:04}"));
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("f"), format!("{i}\n")).unwrap();
    }
    std::fs::write(dir.join("b"), "b\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "one"]);
    std::fs::write(dir.join("b"), "b2\n").unwrap();
    git(&["add", "b"]);
    std::fs::write(dir.join("b"), "b3\n").unwrap();
    let old = std::time::UNIX_EPOCH + std::time::Duration::from_secs(946_684_801);
    let f = std::fs::File::options().write(true).open(dir.join("b")).unwrap();
    f.set_modified(old).unwrap();
    dir
}

#[test]
fn the_worktree_side_wins_over_a_late_tree_index_item() {
    let Some(stock) = stock_git() else { return };
    let (s, z) = (fixture(stock, "s"), fixture(stock, "z"));
    let vectors: &[&[&str]] = &[
        &["diff", "HEAD"],
        &["diff", "HEAD", "--raw"],
        &["diff", "HEAD", "--stat"],
        &["diff", "HEAD", "--name-status"],
        &["diff", "HEAD", "-R"],
        &["diff", "HEAD", "--", "b"],
        &["diff", "HEAD", "--exit-code"],
        &["diff-index", "HEAD"],
        &["diff-index", "-p", "HEAD"],
    ];
    // Several rounds: the order the two threads report in is a matter of timing.
    for _ in 0..5 {
        for args in vectors {
            assert_eq!(run(BIN, &z, args), run(stock, &s, args), "{args:?}");
        }
    }
    let porcelain = ["status", "--porcelain"];
    assert_eq!(run(stock, &z, &porcelain), run(stock, &s, &porcelain), "status");
    let _ = (std::fs::remove_dir_all(s), std::fs::remove_dir_all(z));
}
