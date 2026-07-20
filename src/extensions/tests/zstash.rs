//! `zstash` parks uncommitted tree-wide work and `zunstash` restores it.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(["-c", "user.email=t@e.x", "-c", "user.name=t"]).args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

fn zvcs(home: &Path, cwd: &Path, args: &[&str]) -> (String, bool) {
    let out = Command::new(BIN).args(args).current_dir(cwd).env("ZVCS_HOME", home).output().unwrap();
    (String::from_utf8_lossy(&out.stdout).into_owned(), out.status.success())
}

fn porcelain(dir: &Path) -> String {
    String::from_utf8(Command::new("git").args(["status", "--porcelain"]).current_dir(dir).output().unwrap().stdout).unwrap()
}

#[test]
fn zstash_parks_and_zunstash_restores() {
    let root = std::env::temp_dir().join(format!("zvcs-zstash-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();

    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), b"1\n").unwrap();
    git(&repo, &["add", "a.txt"]);
    git(&repo, &["commit", "-q", "-m", "c0"]);

    // Dirty a tracked file.
    std::fs::write(repo.join("a.txt"), b"2\n").unwrap();
    assert!(!porcelain(&repo).is_empty(), "precondition: repo should be dirty");

    // Stash the tree.
    let (out, ok) = zvcs(&home, &repo, &["zstash"]);
    assert!(ok && out.contains("stashed 1 repo"), "zstash: {out}");
    assert!(porcelain(&repo).is_empty(), "worktree must be clean after zstash");
    assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap(), "1\n", "file reverted to committed state");

    // zstashes lists it.
    assert!(zvcs(&home, &repo, &["zstashes"]).0.contains("wip"), "zstashes should list 'wip'");

    // Restore.
    let (out2, ok2) = zvcs(&home, &repo, &["zunstash"]);
    assert!(ok2 && out2.contains("restored 1 repo"), "zunstash: {out2}");
    assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap(), "2\n", "WIP must be restored");

    let _ = std::fs::remove_dir_all(&root);
}
