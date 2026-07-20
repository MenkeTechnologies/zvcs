//! `zworktree add` provisions an isolated, object-sharing worktree of the whole
//! submodule tree in one command; `remove` tears it down.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args(["-c", "user.email=t@e.x", "-c", "user.name=t", "-c", "protocol.file.allow=always"])
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@e.x")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@e.x")
            .current_dir(dir)
            .status()
            .unwrap()
            .success(),
        "git {args:?} failed"
    );
}

fn out(dir: &Path, args: &[&str]) -> String {
    String::from_utf8(Command::new("git").args(args).current_dir(dir).output().unwrap().stdout)
        .unwrap()
        .trim()
        .to_string()
}

fn zvcs(home: &Path, cwd: &Path, args: &[&str]) -> bool {
    Command::new(BIN).args(args).current_dir(cwd).env("ZVCS_HOME", home).status().unwrap().success()
}

#[test]
fn zworktree_isolated_tree_add_and_remove() {
    let root = std::env::temp_dir().join(format!("zvcs-zwt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");

    // Submodule source + parent with the submodule and a tracked file.
    let sub_src = root.join("sub_src");
    std::fs::create_dir_all(&sub_src).unwrap();
    git(&sub_src, &["init", "-q", "-b", "main"]);
    std::fs::write(sub_src.join("s.txt"), b"s\n").unwrap();
    git(&sub_src, &["add", "s.txt"]);
    git(&sub_src, &["commit", "-q", "-m", "s0"]);

    let super_ = root.join("super");
    std::fs::create_dir_all(&super_).unwrap();
    git(&super_, &["init", "-q", "-b", "main"]);
    std::fs::write(super_.join("top.txt"), b"top\n").unwrap();
    git(&super_, &["add", "top.txt"]);
    git(&super_, &["commit", "-q", "-m", "p0"]);
    git(&super_, &["submodule", "add", "-q", sub_src.to_str().unwrap(), "sub"]);
    git(&super_, &["commit", "-q", "-m", "add sub"]);

    let orig_sub_head = out(&super_.join("sub"), &["rev-parse", "HEAD"]);

    // Provision an isolated worktree of the whole tree.
    assert!(zvcs(&home, &super_, &["zworktree", "add", "agent1"]), "zworktree add failed");
    let wt = home.join("worktrees").join("agent1");
    assert!(wt.exists(), "worktree dir missing");
    assert_eq!(std::fs::read_to_string(wt.join("top.txt")).unwrap(), "top\n", "parent file not checked out");
    assert_eq!(std::fs::read_to_string(wt.join("sub/s.txt")).unwrap(), "s\n", "submodule not checked out");

    // Object sharing: the worktree's .git are *files* (pointers), not object stores.
    assert!(wt.join(".git").is_file(), "parent .git must be a worktree pointer file");
    assert!(wt.join("sub/.git").is_file(), "submodule .git must be a worktree pointer file");
    assert!(!wt.join(".git/objects").exists(), "worktree must not have its own object store");

    // Recognized as a git worktree, on its own branch.
    assert_eq!(out(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]), "zwt/agent1", "wrong worktree branch");

    // Isolation: committing in the worktree's submodule must not move the original.
    std::fs::write(wt.join("sub/s.txt"), b"changed\n").unwrap();
    git(&wt.join("sub"), &["commit", "-qam", "s1 in worktree"]);
    assert_ne!(out(&wt.join("sub"), &["rev-parse", "HEAD"]), orig_sub_head, "worktree sub should have moved");
    assert_eq!(out(&super_.join("sub"), &["rev-parse", "HEAD"]), orig_sub_head, "original sub must be unchanged");

    // list shows it.
    let listing = String::from_utf8(
        Command::new(BIN).args(["zworktree", "list"]).current_dir(&super_).env("ZVCS_HOME", &home).output().unwrap().stdout,
    )
    .unwrap();
    assert!(listing.contains("agent1"), "zworktree list missing agent1:\n{listing}");

    // remove tears it down.
    assert!(zvcs(&home, &super_, &["zworktree", "remove", "agent1"]), "zworktree remove failed");
    assert!(!wt.exists(), "worktree dir must be gone after remove");

    let _ = std::fs::remove_dir_all(&root);
}
