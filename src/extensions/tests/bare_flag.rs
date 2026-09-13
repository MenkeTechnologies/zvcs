//! `git --bare` (git.c:256-263, git v2.55.0): `is_bare_repository_cfg = 1`,
//! `GIT_DIR` = cwd, `GIT_IMPLICIT_WORK_TREE=0`. Expectations were taken from
//! stock git 2.55.0 on the same inputs.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_IMPLICIT_WORK_TREE")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .output()
        .expect("run zvcs git")
}

fn ok(dir: &Path, home: &Path, args: &[&str]) {
    let out = run(dir, home, args);
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// A non-bare repository with one commit beside an empty `$HOME`, named per
/// test and per pid so concurrent binaries never share it.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-bare-flag-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let work = root.join("r");
    std::fs::create_dir_all(&home).expect("mkdir home");
    std::fs::create_dir_all(&work).expect("mkdir work");
    ok(&work, &home, &["init", "-q"]);
    std::fs::write(work.join("a"), "a\n").expect("write a");
    ok(&work, &home, &["add", "a"]);
    ok(&work, &home, &["commit", "-qm", "a"]);
    (root, home)
}

/// With `core.worktree` and no `core.bare`, only the flag makes
/// `is_bare_repository_cfg > 0`, which is what reaches the warning arm of
/// `setup_explicit_git_dir()` (setup.c:1144-1149) and then `setup_work_tree()`'s
/// bogus-config die (setup.c:500-501).
#[test]
fn bare_flag_with_core_worktree_is_bogus_config() {
    let (root, home) = fixture("bogus");
    let git_dir = root.join("r/.git");
    ok(&git_dir, &home, &["config", "core.worktree", ".."]);
    ok(&git_dir, &home, &["config", "--unset", "core.bare"]);

    let out = run(&git_dir, &home, &["--bare", "rev-parse", "--is-bare-repository"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "true\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "warning: core.bare and core.worktree do not make sense\n"
    );

    let out = run(&git_dir, &home, &["--bare", "status"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "warning: core.bare and core.worktree do not make sense\n\
         fatal: unable to set up work tree using invalid config\n"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// `core.bare = false` replaces the flag, and then `GIT_IMPLICIT_WORK_TREE=0`
/// (setup.c:1172-1177) is what keeps the cwd from becoming the work tree.
#[test]
fn bare_flag_inside_git_dir_has_no_implicit_work_tree() {
    let (root, home) = fixture("implicit");
    let git_dir = root.join("r/.git");

    let out = run(&git_dir, &home, &["--bare", "rev-parse", "--is-bare-repository", "--is-inside-work-tree"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "false\nfalse\n");

    let out = run(&git_dir, &home, &["--bare", "status"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(String::from_utf8_lossy(&out.stderr), "fatal: this operation must be run in a work tree\n");
    let _ = std::fs::remove_dir_all(&root);
}
