//! `git log <path>` without an explicit `--`: git treats a positional token that is
//! not a revision but names an existing (or tracked) path as an implicit pathspec, so
//! `git log .` == `git log -- .`. Previously the shadow rejected it with "ambiguous
//! argument '.'". A token that is neither a revision nor a path is still the fatal.

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run binary")
}

fn lines(cwd: &Path, home: &Path, args: &[&str]) -> usize {
    let out = run(cwd, home, args);
    assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).lines().count()
}

#[test]
fn positional_path_is_an_implicit_pathspec() {
    let root = std::env::temp_dir().join(format!("zvcs-logpath-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(repo.join("sub")).unwrap();

    run(&repo, &home, &["init", "-q", "-b", "main"]);
    run(&repo, &home, &["config", "user.email", "t@e.co"]);
    run(&repo, &home, &["config", "user.name", "t"]);

    // c0 touches only top-level `a`; c1 and c2 touch only `sub/b`.
    std::fs::write(repo.join("a"), "a\n").unwrap();
    run(&repo, &home, &["add", "a"]);
    run(&repo, &home, &["commit", "-q", "-m", "c0-a"]);
    for m in ["c1-sub", "c2-sub"] {
        std::fs::write(repo.join("sub/b"), format!("{m}\n")).unwrap();
        run(&repo, &home, &["add", "sub/b"]);
        run(&repo, &home, &["commit", "-q", "-m", m]);
    }

    // `.` names the whole tree → every commit, and must equal the explicit `-- .`.
    assert_eq!(lines(&repo, &home, &["log", "--oneline", "."]), 3, "log . should show all 3");
    assert_eq!(
        lines(&repo, &home, &["log", "--oneline", "."]),
        lines(&repo, &home, &["log", "--oneline", "--", "."]),
        "`git log .` must equal `git log -- .`"
    );

    // A directory pathspec limits history to commits that touched it.
    assert_eq!(lines(&repo, &home, &["log", "--oneline", "sub"]), 2, "log sub → only the 2 sub commits");
    assert_eq!(lines(&repo, &home, &["log", "--oneline", "a"]), 1, "log a → only c0");

    // A revision tip plus a path still walks from the tip, path-limited.
    assert_eq!(lines(&repo, &home, &["log", "--oneline", "HEAD", "sub"]), 2);

    // A path deleted from the worktree but still in the index (unstaged deletion)
    // resolves via the index fallback, just as git's verify_filename does.
    std::fs::remove_file(repo.join("a")).unwrap();
    assert_eq!(lines(&repo, &home, &["log", "--oneline", "a"]), 1, "deleted-but-tracked path still logs");

    // Neither a revision nor a path → still the fatal, exit 128.
    let bad = run(&repo, &home, &["log", "--oneline", "not_a_rev_or_path"]);
    assert_eq!(bad.status.code(), Some(128));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("ambiguous argument"));

    let _ = std::fs::remove_dir_all(&root);
}
