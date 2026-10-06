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

/// Reinitializing a repository whose git directory is read-only: the two
/// gentle unsets `initialize_repository_version()` makes on a reinit
/// (setup.c:2468-2469, :2482-2483) each fail to take the config lock and say so
/// (config.c:3069-3071) without dying, and the version write that follows
/// prints the same line before `repo_config_set()` dies. zvcs swallowed all
/// three lines and printed only the fatal one.
#[test]
fn a_read_only_git_dir_reports_each_config_lock_before_the_fatal() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("zvcs-init-read-only-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-q", "r"]);
    let dot_git = root.join("r/.git");
    std::fs::set_permissions(&dot_git, std::fs::Permissions::from_mode(0o555)).unwrap();
    // A superuser writes through the mode; nothing to observe there.
    if std::fs::File::create(dot_git.join("probe")).is_err() {
        let (out, err, code) = git(&root, &["init", "r"]);
        let config = dot_git.canonicalize().unwrap().join("config");
        let lock = format!("error: could not lock config file {}: Permission denied\n", config.display());
        assert_eq!((out.as_str(), code), ("", 128));
        assert_eq!(err, format!("{lock}{lock}{lock}fatal: could not set 'core.repositoryformatversion' to '0'\n"));
    }
    std::fs::set_permissions(&dot_git, std::fs::Permissions::from_mode(0o755)).unwrap();
    let _ = std::fs::remove_dir_all(&root);
}
