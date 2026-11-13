//! `git diff --no-index <dir> <dir> [<pathspec>...]` against stock git.
//!
//! `queue_diff()` (diff-no-index.c:161-290) recurses one directory level at a
//! time: each level's two listings are sorted by `strcmp()` on the entry names
//! and merged, so `a/x` comes before `a-b` (the entry `a` sorts first), where a
//! sort of whole relative paths puts `a-b` first. A file facing a directory of
//! the same name is queued as deleted (or created) on its own, and the directory
//! is then walked against nothing (diff-no-index.c:174-194); `-R` swaps only the
//! leaf pairs (diff-no-index.c:279-283), so that lone file keeps its side.
//!
//! Operands past two directories are a pathspec (diff-no-index.c:394-399),
//! tested per entry by `match_leading_pathspec()` (diff-no-index.c:50-53), with
//! `:(top)` and `:(attr)` refused and a path that climbs out of the tree fatal.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

type Outcome = (String, String, Option<i32>);

fn run(bin: &str, dir: &Path, args: &[&str]) -> Outcome {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CEILING_DIRECTORIES", dir.parent().unwrap())
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (text(&out.stdout), text(&out.stderr), out.status.code())
}

struct Tree(PathBuf);

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tree(tag: &str) -> Tree {
    let base = std::env::temp_dir().join(format!("zvcs-ni-walk-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let w = |rel: &str, body: &str| {
        let p = base.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    };
    w("l/a/x", "1\n");
    w("r/a/x", "2\n");
    w("l/a-b", "ab\n");
    w("r/a-b", "ab2\n");
    // `e`: a file on the left, a directory on the right; `d` the other way round.
    w("l/e", "f\n");
    w("r/e/deep/f", "inner\n");
    w("l/d/one", "dd\n");
    w("r/d", "dfile\n");
    w("l/sub/s2/q.txt", "q\n");
    w("r/sub/s2/q.txt", "Q\n");
    w("l/sub/w.c", "w\n");
    w("r/sub/w.c", "W\n");
    Tree(base)
}

#[test]
fn walk_order_and_file_against_directory() {
    let Some(stock) = stock_git::stock_git() else { return };
    let t = tree("order");
    for args in [
        &["diff", "--no-index", "--name-status", "l", "r"][..],
        &["diff", "--no-index", "-R", "--name-status", "l", "r"],
        &["diff", "--no-index", "--raw", "l/", "r/"],
        &["diff", "--no-index", "l", "r"],
        &["diff", "--no-index", "--stat", "l/d", "r/d"],
        &["diff", "--no-index", "-R", "l/e", "r/e"],
    ] {
        let want = run(stock, &t.0, args);
        assert_eq!(run(BIN, &t.0, args), want, "{args:?}");
    }
}

#[test]
fn pathspec_limits_the_walk() {
    let Some(stock) = stock_git::stock_git() else { return };
    let t = tree("spec");
    for spec in [
        "sub",
        "sub/*",
        ":(glob)**/q.txt",
        ":!sub",
        ":(icase)SUB",
        ":(literal)a*",
        ":(prefix:1)a",
        ":(prefix:5)x",
        "a?b",
        "nothere",
        ":(top)sub",
        ":(attr:foo)sub",
        "../x",
        "",
    ] {
        let args = ["diff", "--no-index", "--name-status", "l", "r", spec];
        let want = run(stock, &t.0, &args);
        assert_eq!(run(BIN, &t.0, &args), want, "{args:?}");
    }
    // Two elements, and a pathspec after a pair that is not two directories.
    for args in [
        &["diff", "--no-index", "--name-status", "l", "r", "e", "d"][..],
        &["diff", "--no-index", "l/sub/w.c", "r/sub/w.c", "sub"],
    ] {
        let want = run(stock, &t.0, args);
        assert_eq!(run(BIN, &t.0, args), want, "{args:?}");
    }
}
