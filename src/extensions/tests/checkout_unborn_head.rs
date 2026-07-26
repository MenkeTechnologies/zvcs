//! `git checkout <commit>` in a repository whose `HEAD` is still unborn.
//!
//! `git init && git fetch --depth 1 <remote> <sha> && git checkout <sha>` is how
//! tree-sitter grammar fetchers (helix, zmax) populate a grammar source tree, and
//! it is the first checkout of a repo that has neither an index file nor a born
//! `HEAD`. Loading the "current" index by peeling `HEAD` fails there, so the
//! checkout has to fall back to an empty index instead of reporting
//! `Branch 'refs/heads/main' does not have any commits`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap()
}

fn git_ok(dir: &Path, args: &[&str]) -> String {
    let out = git(dir, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

/// A source repo with one commit, plus an empty directory to fetch it into.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-unborn-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();

    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    git_ok(&src, &["init", "-q", "-b", "main"]);
    git_ok(&src, &["config", "user.email", "t@e.x"]);
    git_ok(&src, &["config", "user.name", "t"]);
    std::fs::write(src.join("grammar.js"), "module.exports = {};\n").unwrap();
    git_ok(&src, &["add", "grammar.js"]);
    git_ok(&src, &["commit", "-q", "-m", "c0"]);

    let dst = root.join("dst");
    std::fs::create_dir_all(&dst).unwrap();
    (src, dst)
}

#[test]
fn checkout_detaches_from_unborn_head_and_writes_the_worktree() {
    let (src, dst) = fixture("detach");
    let rev = git_ok(&src, &["rev-parse", "HEAD"]);

    git_ok(&dst, &["init", "-q"]);
    git_ok(&dst, &["fetch", src.to_str().unwrap(), &rev]);

    let out = git(&dst, &["checkout", &rev]);
    assert!(
        out.status.success(),
        "checkout from an unborn HEAD failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(git_ok(&dst, &["rev-parse", "HEAD"]), rev, "HEAD detached at the fetched commit");
    assert_eq!(
        std::fs::read_to_string(dst.join("grammar.js")).unwrap(),
        "module.exports = {};\n",
        "the commit's tree was written to the worktree"
    );
}
