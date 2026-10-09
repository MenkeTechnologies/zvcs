//! `git stash create` when the index has lost paths `HEAD` has.
//!
//! `stash_working_tree()` builds the worktree tree `W` from `diff-index HEAD` against the
//! worktree: a path `HEAD` holds that the index no longer lists — after `git rm --cached`,
//! or with no index file at all, which `repo_read_index()` reads as an empty index — is
//! reported too, and `update-index --add --remove` takes whatever the worktree has there
//! back into `W`. The index tree `I` stays what the index holds. A missing index file is
//! also written out (empty, with its cache-tree) by `write_index_as_tree()`.
//!
//! zvcs died with `An IO error occurred while opening the index` on the missing file and,
//! after `rm --cached`, left the file out of `W`.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, envs: &[(&str, &str)], args: &[&str]) -> (String, String, Option<i32>) {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("HOME", dir)
        .env("LC_ALL", "C");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).trim_end().to_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
    )
}

fn fixture(tag: &str, stock: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-stashidx-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let dir = dir.canonicalize().unwrap();
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    std::fs::write(dir.join("b.txt"), "b\n").unwrap();
    std::fs::write(dir.join("sub/c.txt"), "c\n").unwrap();
    run(stock, &dir, &[], &["init", "-q", "-b", "main"]);
    run(stock, &dir, &[], &["add", "."]);
    assert_eq!(run(stock, &dir, &[], &["commit", "-qm", "one"]).2, Some(0));
    std::fs::write(dir.join("a.txt"), "a edited\n").unwrap();
    dir
}

/// What `stash create` left behind, shape-only: the exit status and stderr, the trees of the
/// stash commit and of its index commit, and the bytes of the index file it wrote.
fn create_and_inspect(
    stock: &str,
    bin: &str,
    dir: &Path,
    envs: &[(&str, &str)],
    index_file: &Path,
) -> (Option<i32>, String, String, String, Vec<u8>) {
    let (out, err, code) = run(bin, dir, envs, &["stash", "create"]);
    if code != Some(0) || out.is_empty() {
        return (code, err, out, String::new(), Vec::new());
    }
    let w = run(stock, dir, &[], &["ls-tree", "-r", &out]).0;
    let i = run(stock, dir, &[], &["ls-tree", "-r", &format!("{out}^2")]).0;
    (code, err, w, i, std::fs::read(index_file).unwrap_or_default())
}

#[test]
fn a_missing_index_file_is_an_empty_index_and_the_worktree_tree_still_holds_head_paths() {
    let Some(stock) = stock_git() else { return };
    let mut seen = Vec::new();
    for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
        let dir = fixture(&format!("missing-{who}"), stock);
        let missing = dir.join(".git/no-such-index");
        let env = [("GIT_INDEX_FILE", missing.to_str().unwrap())];
        seen.push(create_and_inspect(stock, bin, &dir, &env, &missing));
        // The default index file, removed outright.
        let default = dir.join(".git/index");
        std::fs::remove_file(&default).unwrap();
        seen.push(create_and_inspect(stock, bin, &dir, &[], &default));
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert_eq!(seen[..2], seen[2..]);
}

#[test]
fn a_path_removed_from_the_index_only_is_still_in_the_worktree_tree() {
    let Some(stock) = stock_git() else { return };
    let mut seen = Vec::new();
    for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
        let dir = fixture(&format!("rmcached-{who}"), stock);
        run(stock, &dir, &[], &["rm", "-q", "--cached", "b.txt", "sub/c.txt"]);
        seen.push(create_and_inspect(stock, bin, &dir, &[], &dir.join(".git/index")));
        let _ = std::fs::remove_dir_all(&dir);
    }
    // The index file keeps the entries' stat data, which differs between two fixtures.
    let shape = |s: &(Option<i32>, String, String, String, Vec<u8>)| (s.0, s.1.clone(), s.2.clone(), s.3.clone());
    assert_eq!(shape(&seen[0]), shape(&seen[1]));
}
