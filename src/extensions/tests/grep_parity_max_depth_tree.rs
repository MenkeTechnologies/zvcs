//! `--max-depth` is a property of the pathspec, not of one walk: `cmd_grep`
//! writes `pathspec.max_depth` once (builtin/grep.c:1316) and `grep_tree()`'s
//! `tree_entry_interesting()` folds `within_depth()` (dir.c:296) into the same
//! verdict `grep_cache()` gets. The port applied it only to the index/worktree
//! walk, so `git grep --max-depth 0 <pattern> HEAD` searched the whole tree.
//!
//! Measured against git 2.55.0.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new(BIN).args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

/// One match per level of a four-deep tree, so each `--max-depth` value cuts at a
/// different place.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-grepdepth-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(repo.join("a/b/c")).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@e.x"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("top.txt"), "needle\n").unwrap();
    std::fs::write(repo.join("a/x.txt"), "needle\n").unwrap();
    std::fs::write(repo.join("a/b/y.txt"), "needle\n").unwrap();
    std::fs::write(repo.join("a/b/c/z.txt"), "needle\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "c0"]);
    (repo, home)
}

fn grep(repo: &Path, home: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["grep", "--threads", "1"];
    args.extend_from_slice(extra);
    Command::new(BIN)
        .args(&args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .output()
        .unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn tree_search_honors_max_depth_from_the_repository_root() {
    let (repo, home) = fixture("root");
    let d = stdout(&grep(&repo, &home, &["--max-depth", "0", "needle", "HEAD"]));
    assert_eq!(d, "HEAD:top.txt:needle\n", "depth 0 keeps only the slashless paths:\n{d}");
    let d = stdout(&grep(&repo, &home, &["--max-depth", "1", "needle", "HEAD"]));
    assert_eq!(d, "HEAD:a/x.txt:needle\nHEAD:top.txt:needle\n", "depth 1:\n{d}");
    let d = stdout(&grep(&repo, &home, &["--max-depth", "2", "needle", "HEAD"]));
    assert_eq!(
        d, "HEAD:a/b/y.txt:needle\nHEAD:a/x.txt:needle\nHEAD:top.txt:needle\n",
        "depth 2:\n{d}"
    );
}

#[test]
fn tree_search_measures_depth_from_the_pathspec() {
    let (repo, home) = fixture("spec");
    // `within_depth()` starts counting after the matched pathspec literal, so
    // `-- a` with depth 0 keeps `a/x.txt` and drops `a/b/y.txt`.
    let d = stdout(&grep(&repo, &home, &["--max-depth", "0", "needle", "HEAD", "--", "a"]));
    assert_eq!(d, "HEAD:a/x.txt:needle\n", "depth is relative to `a`:\n{d}");
    let d = stdout(&grep(&repo, &home, &["--max-depth", "1", "needle", "HEAD", "--", "a"]));
    assert_eq!(d, "HEAD:a/b/y.txt:needle\nHEAD:a/x.txt:needle\n", "one level under `a`:\n{d}");
}

#[test]
fn tree_and_worktree_walks_cut_at_the_same_place() {
    let (repo, home) = fixture("agree");
    for depth in ["0", "1", "2"] {
        let tree = stdout(&grep(&repo, &home, &["--max-depth", depth, "needle", "HEAD"]));
        let work = stdout(&grep(&repo, &home, &["--max-depth", depth, "needle"]));
        assert_eq!(
            tree,
            work.lines().map(|l| format!("HEAD:{l}\n")).collect::<String>(),
            "one pathspec.max_depth feeds both walks (depth {depth}):\n{tree}vs\n{work}"
        );
    }
}
