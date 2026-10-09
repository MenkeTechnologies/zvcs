//! `cherry-pick` in a sparse checkout.
//!
//! `unpack_trees()` seeds its result from the source index, so a path the sparse checkout
//! left out of the work tree keeps `CE_SKIP_WORKTREE` and stays out of it. zvcs rebuilt the
//! index from the new tree with the bit gone, which made `status` report every file outside
//! the cone as deleted (` D outside/…`).

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> (String, String, Option<i32>) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_EDITOR", "true")
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

fn write(dir: &Path, rel: &str, text: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// `main` has `inside/` and `outside/`; `side` changes one file in each; the checkout is
/// sparse over `inside` only.
fn fixture(tag: &str, stock: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-sparsepick-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    run(stock, &dir, &["init", "-q", "-b", "main"]);
    write(&dir, "inside/keep.txt", "keep\n");
    write(&dir, "outside/drop.txt", "drop\n");
    write(&dir, "outside/nested/deep.txt", "deep\n");
    write(&dir, "root.txt", "root\n");
    run(stock, &dir, &["add", "."]);
    run(stock, &dir, &["commit", "-qm", "base"]);
    run(stock, &dir, &["checkout", "-q", "-b", "side"]);
    write(&dir, "inside/keep.txt", "keep side\n");
    write(&dir, "outside/drop.txt", "drop side\n");
    run(stock, &dir, &["commit", "-qam", "side"]);
    run(stock, &dir, &["checkout", "-q", "main"]);
    write(&dir, "root.txt", "root main\n");
    run(stock, &dir, &["commit", "-qam", "main"]);
    assert_eq!(run(stock, &dir, &["sparse-checkout", "set", "--cone", "inside"]).2, Some(0));
    dir
}

fn observe(stock: &str, dir: &Path) -> Vec<(String, String, Option<i32>)> {
    vec![
        run(stock, dir, &["ls-files", "-t"]),
        run(stock, dir, &["status", "--porcelain"]),
        run(stock, dir, &["ls-tree", "-r", "--name-only", "HEAD"]),
    ]
}

#[test]
fn paths_outside_the_cone_stay_skipped() {
    let Some(stock) = stock_git() else { return };
    let cases: [&[&[&str]]; 3] = [
        &[&["cherry-pick", "-n", "main"]],
        &[&["cherry-pick", "side"]],
        &[&["cherry-pick", "-n", "side"]],
    ];
    for steps in cases {
        let mut seen = Vec::new();
        for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
            let dir = fixture(who, stock);
            let mut results = Vec::new();
            for args in steps {
                results.push(run(bin, &dir, args));
            }
            let state = observe(stock, &dir);
            let _ = std::fs::remove_dir_all(&dir);
            seen.push((results, state));
        }
        assert_eq!(seen[1], seen[0], "steps {steps:?}");
    }
}
