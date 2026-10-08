//! `git commit` refusals that come before any work, against stock git.
//!
//! * `--interactive` (the numbered menu) goes through `prepare_index()`, which refreshes the
//!   index and dies on unmerged paths before `interactive_add()` shows a single line.
//! * A pathspec naming nothing git knows is refused by `list_paths()` before
//!   `add_remove_files()` hashes the matched worktree files, so the refused commit leaves
//!   no blob in the object database.
//! * `-i <untracked>` ends at 128 after the same `error:` line, `-o`/plain at 1.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::Path;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

type Outcome = (String, String, Option<i32>);

fn run(bin: &str, dir: &Path, args: &[&str]) -> Outcome {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
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

fn objects(stock: &str, root: &Path) -> String {
    run(stock, root, &["cat-file", "--batch-all-objects", "--batch-check"]).0
}

/// A committed `f`, a modified tracked `f`, an untracked `u`.
fn plain(stock: &str, root: &Path) {
    std::fs::create_dir_all(root).unwrap();
    run(stock, root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("f"), "a\n").unwrap();
    run(stock, root, &["add", "f"]);
    run(stock, root, &["commit", "-qm", "one"]);
    std::fs::write(root.join("f"), "modified content that gets hashed\n").unwrap();
    std::fs::write(root.join("u"), "untracked\n").unwrap();
}

/// `plain` plus a conflicted merge.
fn conflicted(stock: &str, root: &Path) {
    std::fs::create_dir_all(root).unwrap();
    let git = |args: &[&str]| run(stock, root, args);
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(root.join("f"), "a\n").unwrap();
    git(&["add", "f"]);
    git(&["commit", "-qm", "one"]);
    git(&["checkout", "-qb", "side"]);
    std::fs::write(root.join("c"), "side\n").unwrap();
    git(&["add", "c"]);
    git(&["commit", "-qm", "side"]);
    git(&["checkout", "-q", "main"]);
    std::fs::write(root.join("c"), "main\n").unwrap();
    git(&["add", "c"]);
    git(&["commit", "-qm", "main"]);
    git(&["merge", "side"]);
}

fn compare(name: &str, build: fn(&str, &Path), args: &[&str]) -> Outcome {
    let stock = stock_git::stock_git_at_least((2, 56, 0)).expect("caller checked");
    let base = std::env::temp_dir().join(format!("zvcs-commit-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let mut seen = Vec::new();
    for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
        let root = base.join(who);
        build(stock, &root);
        let outcome = run(bin, &root, args);
        seen.push((outcome, objects(stock, &root)));
    }
    let _ = std::fs::remove_dir_all(&base);
    assert_eq!(seen[1], seen[0], "{args:?}");
    seen.remove(0).0
}

#[test]
fn interactive_dies_on_unmerged_paths_before_the_menu() {
    if stock_git::stock_git_at_least((2, 56, 0)).is_none() {
        return;
    }
    let (stdout, stderr, code) = compare("interactive", conflicted, &["commit", "--interactive"]);
    assert_eq!(code, Some(128), "{stderr}");
    assert_eq!(stdout, "U\tc\n");
    assert!(stderr.contains("Committing is not possible because you have unmerged files."));
}

#[test]
fn an_unmatched_pathspec_leaves_no_blob_behind() {
    if stock_git::stock_git_at_least((2, 56, 0)).is_none() {
        return;
    }
    for args in [
        &["commit", "--no-date", "outside/drop.txt", "."][..],
        &["commit", "-o", "outside/drop.txt", "f"],
        &["commit", "-m", "x", "outside/drop.txt", "f"],
    ] {
        let (_, stderr, code) = compare("pathspec", plain, args);
        assert_eq!(code, Some(1), "{args:?}: {stderr}");
        assert!(
            stderr.contains("error: pathspec 'outside/drop.txt' did not match any file(s) known to git"),
            "{args:?}: {stderr}"
        );
    }
}

#[test]
fn include_with_an_unknown_path_exits_128() {
    if stock_git::stock_git_at_least((2, 56, 0)).is_none() {
        return;
    }
    for args in [&["commit", "-i", "u"][..], &["commit", "-i", "f", "u"], &["commit", "-i", "nope"]] {
        let (_, stderr, code) = compare("include", plain, args);
        assert_eq!(code, Some(128), "{args:?}: {stderr}");
    }
}
