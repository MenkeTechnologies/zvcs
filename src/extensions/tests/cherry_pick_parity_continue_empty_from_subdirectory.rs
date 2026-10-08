//! A stopped pick resumed over a resolution that left nothing to commit, against stock git.
//!
//! `continue_single_pick()` / `run_git_commit()` hand the commit to a `git commit` child that
//! inherits the cwd of the verb — which `setup_git_directory()` has already moved to the top
//! of the work tree — so the status report in its refusal names every path from the root even
//! when the user stood in a subdirectory, and `revert --continue` over the same stop refuses
//! the same way instead of recording an empty commit.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

type Outcome = (String, String, Option<i32>);

fn run(bin: &str, dir: &Path, args: &[&str]) -> Outcome {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("LC_ALL", "C")
        .env("GIT_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@x")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (text(&out.stdout), text(&out.stderr), out.status.code())
}

/// A pick of `side` onto `main` conflicts on `f`; the resolution restores `main`'s content, so
/// the pick is empty. `notes.txt` is modified and `scratch` untracked, to put paths in the report.
fn stopped(stock: &str, root: &Path) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    let git = |args: &[&str]| run(stock, root, args);
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(root.join("f"), "base\n").unwrap();
    std::fs::write(root.join("notes.txt"), "n\n").unwrap();
    std::fs::write(root.join("src/lib.rs"), "l\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "base"]);
    git(&["checkout", "-qb", "side"]);
    std::fs::write(root.join("f"), "side\n").unwrap();
    git(&["commit", "-qam", "side"]);
    git(&["checkout", "-q", "main"]);
    std::fs::write(root.join("f"), "main\n").unwrap();
    git(&["commit", "-qam", "main"]);
    let picked = git(&["cherry-pick", "side"]);
    assert_eq!(picked.2, Some(1), "{picked:?}");
    std::fs::write(root.join("f"), "main\n").unwrap();
    git(&["add", "f"]);
    std::fs::write(root.join("notes.txt"), "changed\n").unwrap();
    std::fs::write(root.join("scratch"), "s\n").unwrap();
}

fn check(name: &str, args: &[&str]) {
    let Some(stock) = stock_git::stock_git_at_least((2, 56, 0)) else { return };
    let base = std::env::temp_dir().join(format!("zvcs-cp-empty-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let mut seen = Vec::new();
    for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
        let root = base.join(who);
        stopped(stock, &root);
        let outcome = run(bin, &root.join("src"), args);
        let state = run(stock, &root, &["status", "--porcelain=v2", "--branch"]).0;
        let head = run(stock, &root, &["rev-parse", "HEAD"]).0;
        seen.push((outcome, state, head));
    }
    let _ = std::fs::remove_dir_all(&base);
    assert_eq!(seen[1], seen[0], "{args:?}");
    let report = &seen[0].0;
    assert_eq!(report.2, Some(1), "{args:?}: {report:?}");
    // The report names paths from the work-tree root.
    assert!(report.0.contains("\tmodified:   notes.txt\n"), "{args:?}: {report:?}");
    assert!(report.1.contains("The previous cherry-pick is now empty"), "{args:?}: {report:?}");
}

#[test]
fn cherry_pick_continue_refuses_with_root_relative_paths() {
    check("cp", &["cherry-pick", "--continue"]);
}

#[test]
fn revert_continue_over_a_cherry_pick_stop_refuses_the_same_way() {
    check("rv", &["revert", "--continue"]);
}

/// `A` conflicts and `B` is already in `main`: skipping `A` moves on to `B`, which goes empty
/// and stops in a `git commit` child's refusal — from the work-tree root again.
#[test]
fn skipping_into_an_empty_pick_reports_from_the_root() {
    let Some(stock) = stock_git::stock_git_at_least((2, 56, 0)) else { return };
    let base = std::env::temp_dir().join(format!("zvcs-cp-skip-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let mut seen = Vec::new();
    for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
        let root = base.join(who);
        std::fs::create_dir_all(root.join("src")).unwrap();
        let git = |args: &[&str]| run(stock, &root, args);
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(root.join("f"), "base\n").unwrap();
        std::fs::write(root.join("g"), "base\n").unwrap();
        std::fs::write(root.join("notes.txt"), "n\n").unwrap();
        std::fs::write(root.join("src/lib.rs"), "l\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        git(&["checkout", "-qb", "side"]);
        std::fs::write(root.join("g"), "side\n").unwrap();
        git(&["commit", "-qam", "A"]);
        std::fs::write(root.join("f"), "same\n").unwrap();
        git(&["commit", "-qam", "B"]);
        git(&["checkout", "-q", "main"]);
        std::fs::write(root.join("g"), "main\n").unwrap();
        git(&["commit", "-qam", "g on main"]);
        std::fs::write(root.join("f"), "same\n").unwrap();
        git(&["commit", "-qam", "f on main"]);
        let picked = git(&["cherry-pick", "side~1", "side"]);
        assert_eq!(picked.2, Some(1), "{picked:?}");
        std::fs::write(root.join("notes.txt"), "changed\n").unwrap();
        let outcome = run(bin, &root.join("src"), &["cherry-pick", "--skip"]);
        let state = run(stock, &root, &["status", "--porcelain=v2", "--branch"]).0;
        seen.push((outcome, state));
    }
    let _ = std::fs::remove_dir_all(&base);
    assert_eq!(seen[1], seen[0]);
    assert!(seen[0].0 .0.contains("\tmodified:   notes.txt\n"), "{:?}", seen[0]);
}
