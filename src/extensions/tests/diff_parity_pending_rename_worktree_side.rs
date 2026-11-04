//! `git diff <rev> -M` over a rename staged with `git mv` and edited again in the
//! worktree.
//!
//! `show_modified()` takes the post-image from `get_stat_data()` of the index entry
//! (diff-lib.c:315-347, :399, v2.56.0), so the destination of the pair is the file,
//! read by `diff_populate_filespec()` (diff.c:4062), and `diffcore_rename()` scores
//! the edited file against the tree's source: one line in forty rewritten is `R096`.
//! The index entry `git mv` wrote still names the tree's blob, so a port that let a
//! late tree↔index item replace the worktree side found an exact rename and printed
//! `R100`. That was the intermittent `pending-rename` disagreement under a loaded
//! harness; the large tree ahead of the paths keeps the tree↔index thread busy, which
//! makes the late order the usual one.
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

fn numbered(prefix: &str, edited: &[usize]) -> String {
    (1..=40)
        .map(|i| match edited.contains(&i) {
            true => format!("{prefix} line {i} edited\n"),
            false => format!("{prefix} line {i}\n"),
        })
        .collect()
}

/// 3000 committed directories `a0000/f`..`a2999/f`, then `near.txt` and `far.txt`
/// renamed with `git mv`. `near-renamed.txt` is edited in the worktree only;
/// `far-renamed.txt` is edited and staged, the control whose score comes from the
/// index.
fn fixture(stock: &str, tag: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join(format!("zvcs-pending-rename-worktree-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let git = |args: &[&str]| run(stock, &dir, args);
    git(&["init", "-q", "-b", "main", "."]);
    for i in 0..3000 {
        let sub = dir.join(format!("a{i:04}"));
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("f"), format!("{i}\n")).unwrap();
    }
    std::fs::write(dir.join("near.txt"), numbered("near", &[])).unwrap();
    std::fs::write(dir.join("far.txt"), numbered("far", &[])).unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "seed"]);
    git(&["mv", "near.txt", "near-renamed.txt"]);
    std::fs::write(dir.join("near-renamed.txt"), numbered("near", &[7])).unwrap();
    git(&["mv", "far.txt", "far-renamed.txt"]);
    std::fs::write(dir.join("far-renamed.txt"), numbered("far", &(1..=12).collect::<Vec<_>>())).unwrap();
    git(&["add", "far-renamed.txt"]);
    dir
}

#[test]
fn a_staged_rename_edited_in_the_worktree_is_scored_against_the_file() {
    let Some(stock) = stock_git() else { return };
    let (s, z) = (fixture(stock, "s"), fixture(stock, "z"));
    let vectors: &[&[&str]] = &[
        &["diff", "HEAD", "--find-renames=30", "--name-status"],
        &["diff", "HEAD", "-M", "--raw"],
        &["diff", "HEAD", "-M", "--summary"],
        &["diff", "HEAD", "-M", "--stat"],
        &["diff", "HEAD", "-M", "-p"],
        &["diff-index", "-M", "HEAD"],
    ];
    let expected = run(stock, &s, vectors[0]);
    assert!(expected.0.contains("R096\tnear.txt\tnear-renamed.txt\n"), "{expected:?}");
    // Several rounds: the order the two threads report in is a matter of timing.
    for _ in 0..5 {
        for args in vectors {
            assert_eq!(run(BIN, &z, args), run(stock, &s, args), "{args:?}");
        }
    }
    let _ = (std::fs::remove_dir_all(s), std::fs::remove_dir_all(z));
}
