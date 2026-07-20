//! Refusal / edge-path coverage for the superset verbs.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    let out = Command::new("git")
        .args(["-c", "user.email=t@e.x", "-c", "user.name=t", "-c", "protocol.file.allow=always"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    out
}

fn out(o: std::process::Output) -> String {
    String::from_utf8(o.stdout).unwrap().trim().to_string()
}

fn tmp(tag: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("zvcs-edge-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p.canonicalize().unwrap()
}

#[test]
fn zundo_refuses_at_initial_commit() {
    let root = tmp("undo");
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["commit", "--allow-empty", "-q", "-m", "c0"]);

    // Only the root commit exists → nothing to undo below it.
    let o = Command::new(BIN).args(["zundo"]).current_dir(&repo).env("ZVCS_HOME", &home).output().unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success(), "zundo at initial commit must fail");
    assert!(err.contains("initial commit") || err.contains("nothing to undo"), "wrong refusal:\n{err}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn zbump_refuses_non_fast_forward() {
    let root = tmp("bump");
    let home = root.join("home");
    let sub_src = root.join("sub_src");
    std::fs::create_dir_all(&sub_src).unwrap();
    git(&sub_src, &["init", "-q", "-b", "main"]);
    git(&sub_src, &["commit", "--allow-empty", "-q", "-m", "s0"]);
    let s0 = out(git(&sub_src, &["rev-parse", "HEAD"]));
    git(&sub_src, &["commit", "--allow-empty", "-q", "-m", "s1"]);

    let parent = root.join("parent");
    std::fs::create_dir_all(&parent).unwrap();
    git(&parent, &["init", "-q", "-b", "main"]);
    git(&parent, &["submodule", "add", "-q", sub_src.to_str().unwrap(), "sub"]);
    git(&parent, &["commit", "-q", "-m", "add sub"]); // parent records sub@s1

    // Rewind the submodule to s0 — now its HEAD is NOT a descendant of the
    // recorded pointer (s1), so a bump would be a non-fast-forward.
    git(&parent.join("sub"), &["reset", "--hard", "-q", &s0]);

    let o = Command::new(BIN).args(["zbump"]).current_dir(&parent).env("ZVCS_HOME", &home).output().unwrap();
    let combined = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    assert!(!o.status.success(), "zbump must fail on a non-ff pointer");
    assert!(combined.contains("not a fast-forward"), "wrong refusal:\n{combined}");

    let _ = std::fs::remove_dir_all(&root);
}
