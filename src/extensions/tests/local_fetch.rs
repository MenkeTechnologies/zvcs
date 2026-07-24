//! Native local fetch — reconcile (`zup`/`zsync`) fetches a **local** origin by
//! copying the object graph directly from the sibling repo, with no
//! `git-upload-pack` subprocess and no network. Regression guard for the local
//! file-transport gap (gix's file transport would otherwise spawn upload-pack).

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(["-c", "user.email=t@e.x", "-c", "user.name=t"])
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?} failed in {}", dir.display());
}

fn head(dir: &Path) -> String {
    let out = Command::new("git").args(["rev-parse", "HEAD"]).current_dir(dir).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn zup_fetches_local_origin_natively() {
    let root = std::env::temp_dir().join(format!("zvcs-nlf-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();

    // A source repo with one commit → a bare origin → a clone `b` that tracks it.
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    git(&src, &["init", "-q", "-b", "main"]);
    std::fs::write(src.join("x.txt"), "r1\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-q", "-m", "r1"]);
    git(&root, &["clone", "-q", "--bare", src.to_str().unwrap(), "origin.git"]);
    let origin = root.join("origin.git");
    git(&root, &["clone", "-q", origin.to_str().unwrap(), "b"]);
    let b = root.join("b");

    // Advance origin/main to r2 from a throwaway clone (so `b` is behind).
    git(&root, &["clone", "-q", origin.to_str().unwrap(), "bump"]);
    let bump = root.join("bump");
    std::fs::write(bump.join("x.txt"), "r2\n").unwrap();
    git(&bump, &["add", "."]);
    git(&bump, &["commit", "-q", "-m", "r2"]);
    git(&bump, &["push", "-q", "origin", "HEAD:main"]);
    let r2 = head(&bump);

    assert_ne!(head(&b), r2, "b should start behind origin");

    // `git zup` in b: reconcile → native local fetch (copy objects, advance
    // origin/main) → fast-forward. No upload-pack subprocess is spawned.
    let out = Command::new(BIN)
        .args(["zup"])
        .current_dir(&b)
        .env("ZVCS_HOME", root.join("home"))
        .output()
        .unwrap();
    assert!(out.status.success(), "zup failed: {}", String::from_utf8_lossy(&out.stderr));

    assert_eq!(head(&b), r2, "b did not fast-forward to origin/main via native local fetch");
    assert_eq!(std::fs::read_to_string(b.join("x.txt")).unwrap(), "r2\n", "worktree not updated");

    let _ = std::fs::remove_dir_all(&root);
}
