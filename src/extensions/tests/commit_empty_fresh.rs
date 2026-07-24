//! `git commit --allow-empty` on a freshly-init'd repo (no index file yet) must
//! create the root empty commit, not fail with "opening the index: No such file or
//! directory". Regression for the shim's `open_index` erroring on the missing file.

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run binary")
}

fn stdout(cwd: &Path, home: &Path, args: &[&str]) -> String {
    String::from_utf8_lossy(&run(cwd, home, args).stdout).into_owned()
}

#[test]
fn commit_allow_empty_on_a_fresh_repo() {
    let root = std::env::temp_dir().join(format!("zvcs-emptyfresh-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();

    run(&repo, &home, &["init", "-q", "-b", "main"]);
    run(&repo, &home, &["config", "user.email", "t@e.co"]);
    run(&repo, &home, &["config", "user.name", "t"]);

    // The bug: this errored on the missing index. It must succeed and make a commit.
    let out = run(&repo, &home, &["commit", "--allow-empty", "-m", "root"]);
    assert!(
        out.status.success(),
        "commit --allow-empty on a fresh repo failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(stdout(&repo, &home, &["log", "--oneline"]).lines().count(), 1);
    // The root commit's tree is git's canonical empty tree.
    assert_eq!(
        stdout(&repo, &home, &["rev-parse", "HEAD^{tree}"]).trim(),
        "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
    );

    // A second empty commit (index now exists) and a normal content commit still work.
    assert!(run(&repo, &home, &["commit", "--allow-empty", "-m", "second"]).status.success());
    std::fs::write(repo.join("f"), "hi\n").unwrap();
    run(&repo, &home, &["add", "f"]);
    assert!(run(&repo, &home, &["commit", "-m", "with file"]).status.success());
    assert_eq!(stdout(&repo, &home, &["log", "--oneline"]).lines().count(), 3);

    // Without --allow-empty, a fresh repo with nothing staged is still refused.
    let repo2 = root.join("repo2");
    std::fs::create_dir_all(&repo2).unwrap();
    run(&repo2, &home, &["init", "-q", "-b", "main"]);
    run(&repo2, &home, &["config", "user.email", "t@e.co"]);
    run(&repo2, &home, &["config", "user.name", "t"]);
    let refused = run(&repo2, &home, &["commit", "-m", "nope"]);
    assert!(!refused.status.success(), "empty commit without --allow-empty must be refused");

    let _ = std::fs::remove_dir_all(&root);
}
