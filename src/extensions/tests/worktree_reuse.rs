//! `zworktree remove` must fully prune the linked-worktree metadata AND the
//! `zwt/<name>` branch, so the name is reusable and no orphans linger.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args(["-c", "user.email=t@e.x", "-c", "user.name=t", "-c", "protocol.file.allow=always"])
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap()
            .success(),
        "git {args:?} failed"
    );
}

fn zvcs(home: &Path, cwd: &Path, args: &[&str]) -> bool {
    Command::new(BIN).args(args).current_dir(cwd).env("ZVCS_HOME", home).status().unwrap().success()
}

/// `git rev-parse --verify <ref>` succeeds?
fn ref_exists(dir: &Path, r: &str) -> bool {
    Command::new("git").args(["rev-parse", "--verify", "--quiet", r]).current_dir(dir).output().unwrap().status.success()
}

#[test]
fn zworktree_remove_prunes_metadata_and_branch() {
    let root = std::env::temp_dir().join(format!("zvcs-wtreuse-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");

    let sub_src = root.join("sub_src");
    std::fs::create_dir_all(&sub_src).unwrap();
    git(&sub_src, &["init", "-q", "-b", "main"]);
    git(&sub_src, &["commit", "--allow-empty", "-q", "-m", "s0"]);

    let parent = root.join("parent");
    std::fs::create_dir_all(&parent).unwrap();
    git(&parent, &["init", "-q", "-b", "main"]);
    git(&parent, &["commit", "--allow-empty", "-q", "-m", "p0"]);
    git(&parent, &["submodule", "add", "-q", sub_src.to_str().unwrap(), "sub"]);
    git(&parent, &["commit", "-q", "-m", "add sub"]);

    // add → metadata + branch exist.
    assert!(zvcs(&home, &parent, &["zworktree", "add", "w1"]), "add failed");
    let meta = parent.join(".git/worktrees/w1");
    assert!(meta.exists(), "parent worktree metadata should exist");
    assert!(parent.join(".git/modules/sub/worktrees/w1").exists(), "submodule worktree metadata should exist");
    assert!(ref_exists(&parent, "zwt/w1"), "parent zwt/w1 branch should exist");

    // remove → metadata + branch pruned everywhere.
    assert!(zvcs(&home, &parent, &["zworktree", "remove", "w1"]), "remove failed");
    assert!(!meta.exists(), "parent worktree metadata must be pruned");
    assert!(!parent.join(".git/modules/sub/worktrees/w1").exists(), "submodule worktree metadata must be pruned");
    assert!(!ref_exists(&parent, "zwt/w1"), "parent zwt/w1 branch must be pruned");
    assert!(!home.join("worktrees/w1").exists(), "worktree dir must be gone");

    // name is reusable.
    assert!(zvcs(&home, &parent, &["zworktree", "add", "w1"]), "re-add after remove failed");
    assert!(home.join("worktrees/w1").exists(), "re-added worktree should exist");

    let _ = std::fs::remove_dir_all(&root);
}
