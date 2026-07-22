//! Parity guards for `git checkout`'s newer path-restore options: `--no-overlay`
//! (which, unlike the default overlay mode, deletes pathspec-matched paths absent
//! from the source tree) and `--pathspec-from-file` (pathspecs read from a file,
//! not the argv). These exercise the destructive/overlay branches added to
//! `porcelain/checkout.rs`, verified against a git-built fixture.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

/// A repo whose single commit `c0` tracks only `a.txt`; `b.txt` is then added to
/// the index+worktree so it exists in the index but not in `HEAD`'s tree.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-cokopts-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@e.x"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("a.txt"), "a-committed\n").unwrap();
    git(&repo, &["add", "a.txt"]);
    git(&repo, &["commit", "-q", "-m", "c0"]);
    (repo, home)
}

fn checkout(repo: &Path, home: &Path, args: &[&str]) -> Output {
    let mut a = vec!["checkout"];
    a.extend_from_slice(args);
    Command::new(BIN)
        .args(&a)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .output()
        .unwrap()
}

fn tracked(repo: &Path, path: &str) -> bool {
    Command::new("git")
        .args(["ls-files", "--error-unmatch", path])
        .current_dir(repo)
        .output()
        .unwrap()
        .status
        .success()
}

#[test]
fn overlay_keeps_extra_path_no_overlay_removes_it() {
    let (repo, home) = fixture("overlay");

    // b.txt is in the index (staged) but not in HEAD's tree.
    std::fs::write(repo.join("b.txt"), "b-staged\n").unwrap();
    git(&repo, &["add", "b.txt"]);

    // Overlay (default): restoring from HEAD never removes b.txt.
    let out = checkout(&repo, &home, &["HEAD", "--", "."]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(repo.join("b.txt").exists(), "overlay must keep the untracked-in-tree path");
    assert!(tracked(&repo, "b.txt"), "overlay must keep b.txt in the index");

    // --no-overlay: b.txt matches the pathspec but is absent from HEAD → removed
    // from both the worktree and the index.
    let out = checkout(&repo, &home, &["--no-overlay", "HEAD", "--", "."]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(!repo.join("b.txt").exists(), "--no-overlay must delete b.txt from the worktree");
    assert!(!tracked(&repo, "b.txt"), "--no-overlay must drop b.txt from the index");
    // a.txt (present in the tree) survives untouched.
    assert!(repo.join("a.txt").exists());
    assert!(tracked(&repo, "a.txt"));

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn pathspec_from_file_restores_listed_paths() {
    let (repo, home) = fixture("psff");

    // Dirty the tracked file, then restore it via a pathspec file.
    std::fs::write(repo.join("a.txt"), "a-dirty\n").unwrap();
    let specs = repo.join("specs.txt");
    std::fs::write(&specs, "a.txt\n").unwrap();

    let arg = format!("--pathspec-from-file={}", specs.display());
    let out = checkout(&repo, &home, &[arg.as_str()]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        std::fs::read_to_string(repo.join("a.txt")).unwrap(),
        "a-committed\n",
        "pathspec-from-file must restore a.txt from the index"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
