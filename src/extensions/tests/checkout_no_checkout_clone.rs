//! `git clone --no-checkout <url>` followed by `git checkout <sha>` — the two
//! commands CMake's `FetchContent` populate step runs, and the shape that proved
//! `twoway_merge()`'s `keep_entry()` arm was being reached without an index.
//!
//! The clone leaves `HEAD` at a commit with **no index file and an empty
//! worktree**, so `is_index_unborn()` holds. git reads that as
//! `topts.initial_checkout` and lets every path of the target tree fall through
//! to `merged_entry()`; carrying the old tree forward instead wrote out only the
//! paths the two trees disagreed on and left the rest of the clone empty — a
//! JUCE fetch came out 2082 files short, and a `clap-juce-extensions` fetch lost
//! its `.gitmodules` (`fatal: No url found for submodule path 'clap-libs/clap'`).
//!
//! The third test is the other half of the contract: with a *born* index the
//! carry-forward must stay, or a staged change to a file both branches share is
//! silently thrown away by every switch.

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

fn write(dir: &Path, rel: &str, body: &str) {
    let full = dir.join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, body).unwrap();
}

fn root(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-nocheckout-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root.canonicalize().unwrap()
}

/// Two commits that differ in exactly two paths, over a tree of five. `c0` is
/// the parent, `c1` the tip the clone's `HEAD` lands on.
///
/// Only `added.txt` and `dropped.txt` differ, so a checkout driven by the tree
/// diff alone touches two paths and leaves `keep.txt`, `a/nested.txt` and
/// `a/b/deep.txt` — the majority of the tree — unwritten.
fn fixture(tag: &str) -> (PathBuf, PathBuf, String, String) {
    let root = root(tag);
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    git_ok(&src, &["init", "-q", "-b", "main"]);
    git_ok(&src, &["config", "user.email", "t@e.x"]);
    git_ok(&src, &["config", "user.name", "t"]);

    write(&src, "keep.txt", "keep\n");
    write(&src, "a/nested.txt", "nested\n");
    write(&src, "a/b/deep.txt", "deep\n");
    write(&src, "dropped.txt", "dropped\n");
    git_ok(&src, &["add", "-A"]);
    git_ok(&src, &["commit", "-q", "-m", "c0"]);
    let c0 = git_ok(&src, &["rev-parse", "HEAD"]);

    std::fs::remove_file(src.join("dropped.txt")).unwrap();
    write(&src, "added.txt", "added\n");
    git_ok(&src, &["add", "-A"]);
    git_ok(&src, &["commit", "-q", "-m", "c1"]);
    let c1 = git_ok(&src, &["rev-parse", "HEAD"]);

    (root, src, c0, c1)
}

fn tracked(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = git_ok(dir, &["ls-files"]).lines().map(str::to_owned).collect();
    v.sort();
    v
}

fn on_disk(dir: &Path) -> Vec<String> {
    let mut v = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.file_name().is_some_and(|n| n == ".git") {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
            } else {
                v.push(p.strip_prefix(dir).unwrap().to_string_lossy().into_owned());
            }
        }
    }
    v.sort();
    v
}

#[test]
fn checkout_after_no_checkout_clone_writes_the_whole_target_tree() {
    let (root, src, c0, _c1) = fixture("diff");
    let dst = root.join("dst");

    git_ok(&root, &["clone", "-q", "--no-checkout", src.to_str().unwrap(), "dst"]);
    assert!(!dst.join(".git/index").exists(), "--no-checkout leaves the index unborn");
    assert_eq!(on_disk(&dst), Vec::<String>::new(), "and the worktree empty");

    let out = git(&dst, &["checkout", &c0]);
    assert!(
        out.status.success(),
        "checkout after --no-checkout clone failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let want = vec![
        "a/b/deep.txt".to_string(),
        "a/nested.txt".to_string(),
        "dropped.txt".to_string(),
        "keep.txt".to_string(),
    ];
    // The three paths c0 and c1 agree on are the regression: a tree-diff-driven
    // checkout writes only `dropped.txt` (and unlinks `added.txt`, which was
    // never there), leaving `keep.txt` and both nested paths missing.
    assert_eq!(on_disk(&dst), want, "every path of the target tree is written out");
    assert_eq!(tracked(&dst), want, "and the index names exactly those paths");
    assert_eq!(
        std::fs::read_to_string(dst.join("a/b/deep.txt")).unwrap(),
        "deep\n",
        "with the target tree's content, not the clone's HEAD"
    );
    assert_eq!(git_ok(&dst, &["status", "--porcelain"]), "", "worktree, index and HEAD agree");
}

#[test]
fn checkout_of_the_clones_own_head_still_materializes_the_worktree() {
    let (root, src, _c0, c1) = fixture("same");
    let dst = root.join("dst");

    git_ok(&root, &["clone", "-q", "--no-checkout", src.to_str().unwrap(), "dst"]);

    // `target_tree == cur_tree`: the switch has nothing to carry, but the
    // worktree it would carry into does not exist yet, so the move still runs.
    let out = git(&dst, &["checkout", &c1]);
    assert!(
        out.status.success(),
        "checkout of the clone's own HEAD failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let want = vec![
        "a/b/deep.txt".to_string(),
        "added.txt".to_string(),
        "a/nested.txt".to_string(),
        "keep.txt".to_string(),
    ];
    let mut want = want;
    want.sort();
    assert_eq!(on_disk(&dst), want, "an identical-tree switch still writes the worktree");
    assert_eq!(tracked(&dst), want, "and populates the index");
}

#[test]
fn a_born_index_still_carries_a_staged_change_across_a_switch() {
    let (root, src, _c0, _c1) = fixture("carry");
    let work = root.join("work");
    git_ok(&root, &["clone", "-q", src.to_str().unwrap(), "work"]);
    git_ok(&work, &["config", "user.email", "t@e.x"]);
    git_ok(&work, &["config", "user.name", "t"]);

    // A branch that differs from main only in `added.txt`, so `keep.txt` is a
    // path the two trees agree on — the one `keep_entry()` exists for.
    git_ok(&work, &["checkout", "-q", "-b", "side"]);
    write(&work, "added.txt", "added on side\n");
    git_ok(&work, &["add", "added.txt"]);
    git_ok(&work, &["commit", "-q", "-m", "side"]);
    git_ok(&work, &["checkout", "-q", "main"]);

    write(&work, "keep.txt", "staged\n");
    git_ok(&work, &["add", "keep.txt"]);

    let out = git(&work, &["checkout", "side"]);
    assert!(
        out.status.success(),
        "switch with a staged shared file failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(work.join("keep.txt")).unwrap(),
        "staged\n",
        "the staged change is carried across the switch, not overwritten"
    );
    assert_eq!(
        git_ok(&work, &["diff", "--cached", "--name-only"]),
        "keep.txt",
        "and it is still staged"
    );
}
