//! `zforeach` fans a command across all/subset of indexed repos.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(Command::new("git").args(args).current_dir(dir).status().unwrap().success(), "git {args:?} failed");
}

fn zvcs(home: &Path, cwd: &Path, args: &[&str]) -> String {
    let out = Command::new(BIN).args(args).current_dir(cwd).env("ZVCS_HOME", home).output().unwrap();
    // zforeach prints per-repo groups on stdout.
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn zforeach_runs_across_all_and_subset() {
    let root = std::env::temp_dir().join(format!("zvcs-foreach-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");

    for name in ["alpha", "beta"] {
        let r = root.join(name);
        std::fs::create_dir_all(&r).unwrap();
        git(&r, &["init", "-q", "-b", "main"]);
        git(&r, &["commit", "--allow-empty", "-q", "-m", "c0"]);
    }
    // Index both.
    assert!(Command::new(BIN).args(["zreindex", root.to_str().unwrap()]).current_dir(&root).env("ZVCS_HOME", &home).status().unwrap().success());

    // Across all: both repos appear.
    let all = zvcs(&home, &root, &["zforeach", "--", "git", "rev-parse", "HEAD"]);
    assert!(all.contains("alpha") && all.contains("beta"), "zforeach all:\n{all}");

    // Subset by --repo: only alpha.
    let sub = zvcs(&home, &root, &["zforeach", "--repo", "alpha", "--", "git", "rev-parse", "HEAD"]);
    assert!(sub.contains("alpha"), "subset missing alpha:\n{sub}");
    assert!(!sub.contains("beta"), "subset should exclude beta:\n{sub}");

    let _ = std::fs::remove_dir_all(&root);
}
