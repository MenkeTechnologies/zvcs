//! `git log --alternate-refs` over a lender whose refs include annotated tags,
//! one of them of a tree, against stock git: `handle_commit()` peels the tag
//! tips the lender reports by `%(objectname)` and drops the one with no commit
//! behind it (revision.c:382-475), and every tip is pended under the name
//! `.alternate` (revision.c:1873-1883).
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> (String, String, Option<i32>) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@x")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (text(&out.stdout), text(&out.stderr), out.status.code())
}

#[test]
fn alternate_tag_tips_are_peeled_and_named_alternate() {
    let Some(stock) = stock_git::stock_git_at_least((2, 56, 0)) else { return };
    let root = std::env::temp_dir().join(format!("zvcs-log-altrefs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let git = |dir: &Path, args: &[&str]| {
        let (out, err, code) = run(stock, dir, args);
        assert_eq!(code, Some(0), "{args:?}: {err}");
        out
    };
    git(&root, &["init", "-q", "-b", "main", "A"]);
    let lender = root.join("A");
    git(&lender, &["commit", "-q", "--allow-empty", "-m", "a1"]);
    git(&lender, &["tag", "-a", "t1", "-m", "t"]);
    git(&lender, &["commit", "-q", "--allow-empty", "-m", "a2"]);
    let tree = git(&lender, &["rev-parse", "HEAD^{tree}"]);
    git(&lender, &["tag", "-a", "tt", "-m", "tree", tree.trim()]);
    git(&root, &["clone", "-q", "--shared", "A", "R"]);
    let repo = root.join("R");
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "r1"]);

    for args in [
        &["log", "--alternate-refs", "--oneline"][..],
        &["log", "--oneline", "HEAD", "--not", "--alternate-refs"],
        &["log", "--alternate-refs", "--source", "--format=%h %S"],
    ] {
        assert_eq!(run(BIN, &repo, args), run(stock, &repo, args), "{args:?}");
    }
    let _ = std::fs::remove_dir_all(&root);
}
