//! `git zls` annotates each entry with a two-column git status field (staged,
//! then unstaged) like `eza --git`. This pins the mapping for the status classes
//! that matter — staged-new, staged+unstaged modified, untracked, and a folded
//! directory — so a regression in the gix status walk or the column logic is
//! caught.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?} failed");
}

#[test]
fn zls_shows_two_column_git_status() {
    let root = std::env::temp_dir().join(format!("zvcs-zls-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();

    git(&root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("tracked.txt"), "v1\n").unwrap();
    std::fs::write(root.join("mod.txt"), "old\n").unwrap();
    std::fs::create_dir(root.join("sub")).unwrap();
    std::fs::write(root.join("sub/deep.txt"), "x\n").unwrap();
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-qm", "c0"]);

    // Build each status class:
    std::fs::write(root.join("added.txt"), "new\n").unwrap();
    git(&root, &["add", "added.txt"]); // staged new → "N-"
    std::fs::write(root.join("tracked.txt"), "v1\nv2\n").unwrap(); // unstaged modified → "-M"
    std::fs::write(root.join("mod.txt"), "old\nchange\n").unwrap();
    git(&root, &["add", "mod.txt"]);
    std::fs::write(root.join("mod.txt"), "old\nchange\nmore\n").unwrap(); // staged + unstaged → "MM"
    std::fs::write(root.join("new.txt"), "untracked\n").unwrap(); // untracked → "-N"
    std::fs::write(root.join("sub/deep.txt"), "x\ndeep2\n").unwrap(); // folds → sub/ "-M"

    let out = Command::new(BIN)
        .args(["zls", "-a", root.to_str().unwrap()])
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "zls failed: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8(out.stdout).unwrap();

    let has = |line: &str| stdout.lines().any(|l| l == line);
    assert!(has("N- added.txt"), "staged-new should be `N-`:\n{stdout}");
    assert!(has("MM mod.txt"), "staged+unstaged modified should be `MM`:\n{stdout}");
    assert!(has("-N new.txt"), "untracked should be `-N`:\n{stdout}");
    assert!(has("-M tracked.txt"), "unstaged modified should be `-M`:\n{stdout}");
    assert!(has("-M sub/"), "a dir folds its subtree status → `-M sub/`:\n{stdout}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn zls_outside_repo_omits_git_column() {
    // A plain temp dir that is not a git repo: entries listed with no status field.
    let root = std::env::temp_dir().join(format!("zvcs-zls-nogit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("file.txt"), "hi\n").unwrap();

    let out = Command::new(BIN)
        .args(["zls", root.to_str().unwrap()])
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.lines().any(|l| l == "file.txt"),
        "outside a repo, entries list with no git column:\n{stdout}"
    );

    let _ = std::fs::remove_dir_all(&root);
}
