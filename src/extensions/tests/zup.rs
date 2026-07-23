//! `zup` fetches and fast-forwards a repo to latest `origin/main`, kept attached.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args(["-c", "user.email=t@e.x", "-c", "user.name=t"])
            .args(args)
            .env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@e.x")
            .env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@e.x")
            .current_dir(dir)
            .status()
            .unwrap()
            .success(),
        "git {args:?} failed"
    );
}

fn head(dir: &Path) -> String {
    String::from_utf8(Command::new("git").args(["rev-parse", "HEAD"]).current_dir(dir).output().unwrap().stdout).unwrap().trim().to_string()
}

#[test]
fn zup_fast_forwards_to_latest() {
    let root = std::env::temp_dir().join(format!("zvcs-zup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");

    let bare = root.join("remote.git");
    git(&root, &["init", "-q", "--bare", bare.to_str().unwrap()]);

    // work: c0, push.
    git(&root, &["clone", "-q", bare.to_str().unwrap(), "work"]);
    let work = root.join("work");
    git(&work, &["checkout", "-q", "-B", "main"]);
    git(&work, &["commit", "--allow-empty", "-q", "-m", "c0"]);
    git(&work, &["push", "-q", "origin", "main"]);

    // work2: advance the remote to c1.
    git(&root, &["clone", "-q", bare.to_str().unwrap(), "work2"]);
    let work2 = root.join("work2");
    git(&work2, &["checkout", "-q", "main"]);
    git(&work2, &["commit", "--allow-empty", "-q", "-m", "c1"]);
    git(&work2, &["push", "-q", "origin", "main"]);
    let c1 = head(&work2);

    // work is behind (still c0, unfetched). zup fetches + fast-forwards it.
    assert_ne!(head(&work), c1);
    let out = Command::new(BIN).args(["zup"]).current_dir(&work).env("ZVCS_HOME", &home).output().unwrap();
    let report = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "zup failed: {}{}", report, String::from_utf8_lossy(&out.stderr));

    assert_eq!(head(&work), c1, "zup must fast-forward work to origin/main;\n{report}");
    // Attached to main, not detached.
    let branch = String::from_utf8(Command::new("git").args(["rev-parse", "--abbrev-ref", "HEAD"]).current_dir(&work).output().unwrap().stdout).unwrap();
    assert_eq!(branch.trim(), "main", "HEAD must stay attached to main");

    let _ = std::fs::remove_dir_all(&root);
}
