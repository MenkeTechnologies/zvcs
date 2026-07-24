//! `git log` revision ranges: `A..B` (= `^A B`), `A...B` (symmetric difference,
//! excluding the merge-base), the HEAD-relative `A..`/`..B`, and a leading `^A`.
//! Regression for the shim rejecting ranges as `unknown revision` (which broke
//! `git log v0.4.1..HEAD`). Self-contained — built with the shadow binary, no
//! network, no system git.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(cwd: &Path, home: &Path, args: &[&str]) -> String {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run binary");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The commit subjects `git log --oneline <args>` prints, newest-first.
fn subjects(cwd: &Path, home: &Path, args: &[&str]) -> Vec<String> {
    let mut a = vec!["log", "--oneline"];
    a.extend_from_slice(args);
    run(cwd, home, &a)
        .lines()
        .filter_map(|l| l.split_once(' ').map(|(_, s)| s.to_string()))
        .collect()
}

#[test]
fn log_revision_ranges() {
    let root = std::env::temp_dir().join(format!("zvcs-logrange-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();

    run(&repo, &home, &["init", "-q", "-b", "main"]);
    run(&repo, &home, &["config", "user.email", "t@e.co"]);
    run(&repo, &home, &["config", "user.name", "t"]);
    // Five commits c0..c4 (real content — `commit --allow-empty` on a fresh repo is
    // a separate unported path, so give each commit a change).
    for m in ["c0", "c1", "c2", "c3", "c4"] {
        std::fs::write(repo.join("f"), format!("{m}\n")).unwrap();
        run(&repo, &home, &["add", "f"]);
        run(&repo, &home, &["commit", "-q", "-m", m]);
    }
    run(&repo, &home, &["tag", "v1", "HEAD~3"]); // v1 = c1

    // A..B excludes everything reachable from A.
    assert_eq!(subjects(&repo, &home, &["v1..HEAD"]), ["c4", "c3", "c2"]);
    // ^A B is the same.
    assert_eq!(subjects(&repo, &home, &["^v1", "HEAD"]), ["c4", "c3", "c2"]);
    // HEAD~2..HEAD is the two newest.
    assert_eq!(subjects(&repo, &home, &["HEAD~2..HEAD"]), ["c4", "c3"]);
    // A.. means A..HEAD.
    assert_eq!(subjects(&repo, &home, &["HEAD~4.."]), ["c4", "c3", "c2", "c1"]);
    // An empty range prints nothing.
    assert!(subjects(&repo, &home, &["HEAD..HEAD"]).is_empty());

    // Symmetric difference: diverge a branch from c2 and compare.
    run(&repo, &home, &["checkout", "-q", "-b", "feat", "HEAD~2"]);
    for m in ["f5", "f6"] {
        std::fs::write(repo.join("g"), format!("{m}\n")).unwrap();
        run(&repo, &home, &["add", "g"]);
        run(&repo, &home, &["commit", "-q", "-m", m]);
    }
    let mut sym = subjects(&repo, &home, &["main...feat"]);
    sym.sort();
    // c3, c4 (only on main) and f5, f6 (only on feat); the common c0..c2 are excluded.
    assert_eq!(sym, ["c3", "c4", "f5", "f6"]);

    let _ = std::fs::remove_dir_all(&root);
}
