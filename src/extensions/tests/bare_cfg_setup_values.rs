//! `is_bare_repository_cfg` has two lives in git v2.55.0. Setup reads it from
//! `git --bare` (git.c:258) and the repository's own files
//! (`check_repository_format_gently()`, setup.c:797-801) and decides the work
//! tree with it (setup.c:1144, 1231); `git_default_core_config()`
//! (environment.c:339-342) later replaces it with the last `core.bare` in the
//! full configuration, `-c` included, and that is what `is_bare_repository()`
//! (environment.c:131-135) answers. Expectations were taken from stock git
//! 2.55.0 on the same inputs.

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
        .env_remove("GIT_CONFIG_PARAMETERS")
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

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A non-bare repository with one commit and an untracked file beside an empty
/// `$HOME`, named per test and per pid so concurrent binaries never share it.
fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-bare-cfg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let work = root.join("r");
    std::fs::create_dir_all(&home).expect("mkdir home");
    std::fs::create_dir_all(&work).expect("mkdir work");
    ok(&work, &home, &["init", "-q"]);
    std::fs::write(work.join("a"), "a\n").expect("write a");
    ok(&work, &home, &["add", "a"]);
    ok(&work, &home, &["commit", "-qm", "a"]);
    std::fs::write(work.join("u"), "u\n").expect("write u");
    let work = std::fs::canonicalize(&work).expect("canonicalize work");
    (root, home, work)
}

/// An unset `core.bare` is -1, not > 0, so standing in `.git` with
/// `core.worktree = ..` reaches `setup_explicit_git_dir()`'s `core.worktree` arm
/// (setup.c:1156-1170) and gets the work tree; `-c core.bare=true` is not seen by
/// setup and only makes `is_bare_repository()` consult that work tree.
#[test]
fn unset_core_bare_in_git_dir_uses_core_worktree() {
    let (root, home, work) = fixture("unset-worktree");
    let git_dir = work.join(".git");
    ok(&git_dir, &home, &["config", "--unset", "core.bare"]);
    ok(&git_dir, &home, &["config", "core.worktree", ".."]);
    let expected = format!("false\n{}\n.git/\n", work.display());

    for pre in [&[][..], &["-c", "core.bare=true"][..]] {
        let args = [pre, &["rev-parse", "--is-bare-repository", "--show-toplevel", "--show-prefix"][..]].concat();
        let out = run(&git_dir, &home, &args);
        assert_eq!(stdout(&out), expected, "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(String::from_utf8_lossy(&out.stderr), "", "{args:?}");

        let args = [pre, &["status", "--porcelain"][..]].concat();
        let out = run(&git_dir, &home, &args);
        assert_eq!(stdout(&out), "?? u\n", "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// `--git-dir` with no `core.bare` anywhere ends in `set_git_work_tree(repo, ".")`
/// (setup.c:1178): the cwd is the work tree.
#[test]
fn unset_core_bare_with_git_dir_uses_cwd_as_work_tree() {
    let (root, home, work) = fixture("unset-gitdir");
    ok(&work, &home, &["config", "--unset", "core.bare"]);
    let git_dir = format!("--git-dir={}", work.join(".git").display());

    let out = run(&work, &home, &[&git_dir, "rev-parse", "--is-bare-repository", "--show-toplevel"]);
    assert_eq!(stdout(&out), format!("false\n{}\n", work.display()));
    let out = run(&work, &home, &[&git_dir, "status", "--porcelain"]);
    assert_eq!(stdout(&out), "?? u\n");
    let _ = std::fs::remove_dir_all(&root);
}

/// The value setup ran with and the value `is_bare_repository()` reads differ
/// whenever `-c core.bare` disagrees with the files or with `--bare`.
#[test]
fn command_line_core_bare_applies_after_setup() {
    let (root, home, work) = fixture("after-setup");
    let git_dir = work.join(".git");

    // Files say false; inside `.git` there is no work tree (`setup_bare_git_dir()`),
    // so `-c core.bare=true` makes it bare.
    let out = run(&git_dir, &home, &["-c", "core.bare=true", "rev-parse", "--is-bare-repository"]);
    assert_eq!(stdout(&out), "true\n");

    // `--bare` with `core.bare` unset: setup is bare (flag = 1), `-c` then says false.
    ok(&git_dir, &home, &["config", "--unset", "core.bare"]);
    let out = run(&git_dir, &home, &["-c", "core.bare=false", "--bare", "rev-parse", "--is-bare-repository"]);
    assert_eq!(stdout(&out), "false\n");

    // Files say true with `core.worktree`: setup warns (setup.c:1147) on the files'
    // value although `-c` says false, and the answer is the `-c` one.
    ok(&git_dir, &home, &["config", "core.bare", "true"]);
    ok(&git_dir, &home, &["config", "core.worktree", ".."]);
    let out = run(&work, &home, &["-c", "core.bare=false", "rev-parse", "--is-bare-repository"]);
    assert_eq!(stdout(&out), "false\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "warning: core.bare and core.worktree do not make sense\n"
    );
    let _ = std::fs::remove_dir_all(&root);
}
