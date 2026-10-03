//! `git submodule status` when the submodule's HEAD is unborn, or does not hold
//! the commit the superproject records.
//!
//! * `ce_compare_gitlink()` (read-cache.c) counts a gitlink whose HEAD does not
//!   resolve as matching, so an unborn submodule is ` <recorded oid> <path>`.
//! * `compute_rev_name()` names the commit with `git describe` children whose
//!   stderr is discarded. For an object the submodule lacks, plain and `--tags`
//!   fail, but `describe --contains` only warns and exits 0 with empty stdout,
//!   so the name is empty and the line ends in ` ()`.
//!
//! zvcs refused the unborn case and printed no name for the missing one.
//! Expectations captured from stock git 2.56.0.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "zvcs-submodule-status-unborn-{name}-{}",
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
        .current_dir(dir)
        .output()
        .expect("run the binary under test");
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    out
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `sup` with submodule `lib` whose checkout is replaced by a fresh repository;
/// returns `(sup, recorded oid)`.
fn fixture(name: &str) -> (PathBuf, String) {
    let root = scratch(name);
    let lib = root.join("lib");
    git(&root, &["init", "-q", "-b", "main", "lib"]);
    git(&lib, &["commit", "-q", "--allow-empty", "-m", "one"]);
    let recorded = stdout(&git(&lib, &["rev-parse", "HEAD"])).trim().to_string();

    let sup = root.join("sup");
    git(&root, &["init", "-q", "-b", "main", "sup"]);
    git(&sup, &["-c", "protocol.file.allow=always", "submodule", "add", "-q", "../lib", "lib"]);
    git(&sup, &["commit", "-q", "-m", "sub"]);

    let sub = sup.join("lib");
    std::fs::remove_dir_all(&sub).unwrap();
    std::fs::create_dir_all(&sub).unwrap();
    git(&sub, &["init", "-q"]);
    (sup, recorded)
}

#[test]
fn an_unborn_submodule_matches_its_gitlink() {
    let (sup, recorded) = fixture("unborn");
    for args in [&["submodule", "status"][..], &["submodule", "status", "--cached"][..]] {
        assert_eq!(stdout(&git(&sup, args)), format!(" {recorded} lib ()\n"), "{args:?}");
    }
}

#[test]
fn a_commit_the_submodule_lacks_gets_an_empty_name() {
    let (sup, recorded) = fixture("missing");
    git(&sup.join("lib"), &["commit", "-q", "--allow-empty", "-m", "other"]);
    let out = git(&sup, &["submodule", "status", "--cached"]);
    assert_eq!(stdout(&out), format!("+{recorded} lib ()\n"));
}
