//! `git init` over a `.git` directory that exists but is not a repository yet.
//!
//! `init_db()` makes the git directory with `safe_create_dir()`, which accepts
//! one that is already there, and `create_default_files()` fills it in; only a
//! readable `HEAD` makes it a reinitialization (`is_reinit()`). So `mkdir -p
//! e/.git && git init e` initializes `e/.git` as a fresh repository, keeping
//! whatever else the directory held. gitoxide refuses any existing `.git`, and
//! zvcs passed that on as `zvcs: init: Refusing to initialize the existing
//! '<path>' directory` at exit 1. Expectations captured from stock git 2.56.0.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("ZVCS_HOME", dir)
        .env("GIT_CEILING_DIRECTORIES", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .expect("run the binary under test");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

#[test]
fn an_existing_dot_git_directory_is_filled_in() {
    let root = std::env::temp_dir().join(format!("zvcs-init-existing-dot-git-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("e/.git/keep")).unwrap();
    let real = root.canonicalize().unwrap();

    let (out, _, code) = git(&root, &["init", "-b", "main", "e"]);
    let want = format!("Initialized empty Git repository in {}/e/.git/\n", real.display());
    assert_eq!((out, code), (want, 0));
    let dot_git = root.join("e/.git");
    assert_eq!(std::fs::read_to_string(dot_git.join("HEAD")).unwrap(), "ref: refs/heads/main\n");
    assert!(dot_git.join("keep").is_dir());
    assert!(dot_git.join("hooks/pre-commit.sample").is_file());
    let config = std::fs::read_to_string(dot_git.join("config")).unwrap();
    assert!(config.contains("\tbare = false\n\tlogallrefupdates = true\n"), "{config}");
    // A work tree repository, not a bare one at `e/.git`.
    let (out, _, code) = git(&root.join("e"), &["rev-parse", "--is-bare-repository", "--git-dir"]);
    assert_eq!((out.as_str(), code), ("false\n.git\n", 0));
    let _ = std::fs::remove_dir_all(&root);
}

/// With `--separate-git-dir`, `separate_git_dir()` first moves such a `.git` to
/// the requested place and leaves a gitfile, and the init fills it in there.
/// zvcs built a second repository in the parent directory's `.git` and then
/// failed to move it onto the one it had already moved: `unable to move
/// <parent>/.git to <sg>: Directory not empty`.
#[test]
fn an_existing_dot_git_directory_moves_to_the_separate_git_dir() {
    let root = std::env::temp_dir().join(format!("zvcs-init-existing-sep-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("e/.git/keep")).unwrap();
    let real = root.canonicalize().unwrap();

    let (out, _, code) = git(&root, &["init", "-b", "main", "--separate-git-dir=sg", "e"]);
    let want = format!("Initialized empty Git repository in {}/sg/\n", real.display());
    assert_eq!((out, code), (want, 0));
    assert_eq!(
        std::fs::read_to_string(root.join("e/.git")).unwrap(),
        format!("gitdir: {}/sg\n", real.display())
    );
    assert!(root.join("sg/keep").is_dir() && root.join("sg/HEAD").is_file());
    assert!(!root.join(".git").exists());
    let _ = std::fs::remove_dir_all(&root);
}
