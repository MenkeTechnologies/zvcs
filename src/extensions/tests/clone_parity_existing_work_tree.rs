//! A `GIT_WORK_TREE` that already exists.
//!
//! builtin/clone.c:1091-1097 reads `GIT_WORK_TREE` for a non-bare clone and
//! dies `working tree '%s' already exists.` when `path_exists()` (a `stat()`)
//! finds it, before the `Cloning into` banner and before anything is created.
//! `git --work-tree=. clone ...` lands there through git.c's `setenv`. zvcs
//! ignored the variable and cloned into the destination at exit 0. A bare
//! clone never reads it. Expectations captured from stock git 2.56.0.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-clone-existing-wt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn git(dir: &Path, args: &[&str]) -> (String, i32) {
    let out = Command::new(BIN)
        .args(args)
        .env("HOME", dir)
        .env("ZVCS_HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .current_dir(dir)
        .output()
        .expect("run the binary under test");
    (String::from_utf8_lossy(&out.stderr).into_owned(), out.status.code().expect("no signal"))
}

#[test]
fn an_existing_work_tree_dies_before_the_banner() {
    let root = scratch();
    git(&root, &["init", "-q", "-b", "main", "src"]);
    git(&root.join("src"), &["commit", "-q", "--allow-empty", "-m", "c1"]);

    assert_eq!(
        git(&root, &["--work-tree=.", "clone", "src", "dst"]),
        ("fatal: working tree '.' already exists.\n".to_string(), 128)
    );
    assert!(!root.join("dst").exists(), "nothing is created before the check");

    assert_eq!(
        git(&root, &["--work-tree=.", "clone", "-q", "--bare", "src", "b.git"]),
        (String::new(), 0),
        "a bare clone never reads GIT_WORK_TREE"
    );
    let _ = std::fs::remove_dir_all(&root);
}
