//! `git check-attr` reads `.gitattributes` from a tree instead of the working
//! tree when one is named. git resolves that source in `compute_default_attr_source`
//! (attr.c): an explicit `--source` first, then `GIT_ATTR_SOURCE`, then the
//! `attr.tree` configuration, and finally the working tree. A value that does not
//! name a tree is ignored rather than fatal.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new(BIN).args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

/// A repository whose committed `.gitattributes` sets `text=auto` on `f.txt`
/// while the working-tree copy sets `binary` instead, so the two sources are
/// distinguishable by a single query.
fn fixture() -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-attrtree-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    let repo = repo.canonicalize().unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@e.x"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join(".gitattributes"), "f.txt text=auto\n").unwrap();
    std::fs::write(repo.join("f.txt"), "hi\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "one"]);
    std::fs::write(repo.join(".gitattributes"), "f.txt binary\n").unwrap();
    (repo, home)
}

fn run(repo: &Path, home: &Path, env: &[(&str, &str)], args: &[&str]) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home);
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

#[test]
fn attr_tree_selects_the_attribute_source() {
    let (repo, home) = fixture();

    // No source named: the working tree's `.gitattributes` wins, so `text` is
    // unset (the committed `text=auto` is not in play).
    let out = run(&repo, &home, &[], &["check-attr", "text", "f.txt"]);
    assert_eq!(stdout(&out), "f.txt: text: unset");

    // `attr.tree` redirects the read to the committed copy.
    let out = run(&repo, &home, &[], &["-c", "attr.tree=HEAD", "check-attr", "text", "f.txt"]);
    assert_eq!(stdout(&out), "f.txt: text: auto");

    // …and the working tree's own rule is then invisible.
    let out = run(&repo, &home, &[], &["-c", "attr.tree=HEAD", "check-attr", "binary", "f.txt"]);
    assert_eq!(stdout(&out), "f.txt: binary: unspecified");

    // A value that does not resolve to a tree is ignored, not fatal.
    let out = run(&repo, &home, &[], &["-c", "attr.tree=no-such-ref", "check-attr", "text", "f.txt"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(stdout(&out), "f.txt: text: unset");

    // `GIT_ATTR_SOURCE` names the same kind of source and applies without config.
    let out = run(&repo, &home, &[("GIT_ATTR_SOURCE", "HEAD")], &["check-attr", "text", "f.txt"]);
    assert_eq!(stdout(&out), "f.txt: text: auto");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
