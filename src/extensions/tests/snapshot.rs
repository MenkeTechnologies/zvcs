//! Tree-wide snapshot/restore: capture the parent + submodule HEADs, advance
//! both, then restore the whole tree back to the snapshot in one command.

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

fn head(dir: &Path) -> String {
    String::from_utf8(Command::new("git").args(["rev-parse", "HEAD"]).current_dir(dir).output().unwrap().stdout)
        .unwrap()
        .trim()
        .to_string()
}

fn zvcs(home: &Path, cwd: &Path, args: &[&str]) -> bool {
    Command::new(BIN).args(args).current_dir(cwd).env("ZVCS_HOME", home).status().unwrap().success()
}

#[test]
fn snapshot_and_restore_tree() {
    let root = std::env::temp_dir().join(format!("zvcs-snap-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");

    // Submodule source + parent with the submodule.
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

    let sub_wt = parent.join("sub");
    let snap_parent = head(&parent);
    let snap_sub = head(&sub_wt);

    // Snapshot the tree.
    assert!(zvcs(&home, &parent, &["zsnapshot", "snap1"]), "zsnapshot failed");

    // Advance both the submodule and the parent.
    git(&sub_wt, &["commit", "--allow-empty", "-q", "-m", "s1"]);
    std::fs::write(parent.join("f.txt"), b"x\n").unwrap();
    git(&parent, &["add", "f.txt"]);
    git(&parent, &["commit", "-q", "-m", "p2"]);
    assert_ne!(head(&parent), snap_parent);
    assert_ne!(head(&sub_wt), snap_sub);

    // Restore the whole tree.
    assert!(zvcs(&home, &parent, &["zrestore", "snap1"]), "zrestore failed");
    assert_eq!(head(&parent), snap_parent, "parent must be restored");
    assert_eq!(head(&sub_wt), snap_sub, "submodule must be restored");

    let _ = std::fs::remove_dir_all(&root);
}
