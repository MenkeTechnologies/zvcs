//! `git submodule status` naming a commit only a later tag reaches.
//!
//! `compute_rev_name()` (builtin/submodule--helper.c) tries four `git describe`
//! children in turn — plain, `--tags`, `--contains`, `--all --always` — and
//! prints the first that exits 0. A commit *behind* the only tag fails the
//! first two and is named by the third, name-rev style: `(v1~1)`. zvcs
//! refused that case outright. Expectations captured from stock git 2.56.0.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "zvcs-submodule-status-contains-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn git(dir: &Path, args: &[&str]) -> Output {
    let out = Command::new(BIN)
        .args(args)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .current_dir(dir)
        .output()
        .expect("run the binary under test");
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    out
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_commit_behind_the_only_tag_is_named_relative_to_it() {
    let root = scratch("behind");
    let lib = root.join("lib");
    git(&root, &["init", "-q", "-b", "main", "lib"]);
    git(&lib, &["commit", "-q", "--allow-empty", "-m", "one"]);
    git(&lib, &["commit", "-q", "--allow-empty", "-m", "two"]);
    git(&lib, &["tag", "v1"]);

    let sup = root.join("sup");
    git(&root, &["init", "-q", "-b", "main", "sup"]);
    git(&sup, &["-c", "protocol.file.allow=always", "submodule", "add", "-q", "../lib", "lib"]);
    git(&sup, &["commit", "-q", "-m", "sub"]);

    let sub = sup.join("lib");
    git(&sub, &["checkout", "-q", "HEAD^"]);
    let behind = stdout(&git(&sub, &["rev-parse", "HEAD"]));

    let out = git(&sup, &["submodule", "status"]);
    assert_eq!(stdout(&out), format!("+{} lib (v1~1)\n", behind.trim()));
    assert!(out.stderr.is_empty(), "{}", String::from_utf8_lossy(&out.stderr));
}
