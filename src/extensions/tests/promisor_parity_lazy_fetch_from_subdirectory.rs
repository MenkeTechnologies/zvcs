//! A lazy fetch from the promisor remote goes where `remote.<name>.url` says, and a relative local
//! URL such as `./.remote.git` is written from the directory git stood in when it spawned the
//! fetch: the top of the work tree, since `setup_git_directory()` has moved there. zvcs fetched in
//! process and resolved the URL against the directory the command was typed in, so a `checkout`,
//! `diff` or `cat-file` that needed a missing blob died `An object with id … could not be found`
//! from any subdirectory.

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn git(bin: &str, dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir.ancestors().nth(1).unwrap_or(dir))
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
        out.status.code().expect("no signal"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `root/par`: a blobless partial clone of a two-branch history whose promisor remote is the
/// relative `./.remote.git`, with an empty `sub` directory to stand in.
fn partial_clone(label: &str, stock: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-lazy-subdir-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let run = |dir: &Path, args: &[&str]| {
        let out = git(stock, dir, args);
        assert_eq!(out.0, 0, "{args:?}: {out:?}");
    };
    run(&root, &["init", "-q", "-b", "main", "src"]);
    let src = root.join("src");
    std::fs::write(src.join("hist.txt"), "v1\n").unwrap();
    run(&src, &["add", "hist.txt"]);
    run(&src, &["commit", "-q", "-m", "one"]);
    run(&src, &["checkout", "-q", "-b", "side"]);
    std::fs::write(src.join("hist.txt"), "v2\n").unwrap();
    run(&src, &["commit", "-q", "-am", "two"]);
    run(&src, &["checkout", "-q", "main"]);
    run(&root, &["clone", "-q", "--bare", "src", "remote.git"]);
    run(&root.join("remote.git"), &["config", "uploadpack.allowFilter", "true"]);
    let url = format!("file://{}", root.join("remote.git").display());
    run(&root, &["clone", "-q", "--no-checkout", "--filter=blob:none", &url, "par"]);
    let par = root.join("par");
    std::fs::rename(root.join("remote.git"), par.join(".remote.git")).unwrap();
    run(&par, &["config", "remote.origin.url", "./.remote.git"]);
    std::fs::create_dir_all(par.join("sub")).unwrap();
    (root, par)
}

fn same(label: &str, from: &str, args: &[&str]) {
    let Some(stock) = stock_git() else { return };
    let mut seen = Vec::new();
    for (bin, side) in [(stock, "stock"), (ZVCS, "zvcs")] {
        let (root, par) = partial_clone(&format!("{label}-{side}"), stock);
        let run = git(bin, &par.join(from), args);
        let shown = root.to_string_lossy().into_owned();
        let file = std::fs::read_to_string(par.join("hist.txt")).ok();
        let head = git(stock, &par, &["rev-parse", "--abbrev-ref", "HEAD"]).1;
        seen.push((run.0, run.1.replace(&shown, "<root>"), run.2.replace(&shown, "<root>"), file, head));
        let _ = std::fs::remove_dir_all(&root);
    }
    assert_eq!(seen[1], seen[0], "git {args:?} from {from:?}: left is zvcs, right is stock");
}

#[test]
fn checkout_fetches_what_it_writes_from_a_subdirectory() {
    same("checkout", "sub", &["checkout", "side"]);
    same("switch", "sub", &["switch", "side"]);
    same("reset", "sub", &["reset", "--hard", "origin/side"]);
}

#[test]
fn reading_commands_fetch_from_a_subdirectory_too() {
    same("cat-file", "sub", &["cat-file", "-p", "origin/side:hist.txt"]);
    same("diff", "sub", &["diff", "origin/main", "origin/side"]);
    same("show", "sub", &["show", "origin/side:hist.txt"]);
}

#[test]
fn the_top_of_the_work_tree_is_unchanged() {
    same("top-checkout", ".", &["checkout", "side"]);
    same("top-cat-file", ".", &["cat-file", "-p", "origin/side:hist.txt"]);
}
