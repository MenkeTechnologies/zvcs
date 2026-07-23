//! `git tag` listing honors `tag.sort` as the default sort order, with `--sort`
//! still overriding. Regression guard for the config being ignored (always
//! ascending refname).

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-tagcfg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@e.x"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("f"), "x\n").unwrap();
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-q", "-m", "c0"]);
    for v in ["v1.0", "v2.0", "v10.0", "v1.5"] {
        git(&repo, &["tag", v]);
    }
    (repo, home)
}

fn tags(repo: &Path, home: &Path, extra: &[&str]) -> Vec<String> {
    let mut args = vec!["tag"];
    args.extend_from_slice(extra);
    let out = Command::new(BIN)
        .args(&args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).split_whitespace().map(String::from).collect()
}

#[test]
fn tag_sort_config_and_override() {
    let (repo, home) = fixture("sort");

    // Default: ascending refname (lexical) — v10.0 before v2.0.
    assert_eq!(tags(&repo, &home, &[]), ["v1.0", "v1.5", "v10.0", "v2.0"]);

    // tag.sort=version:refname → version-aware order.
    git(&repo, &["config", "tag.sort", "version:refname"]);
    assert_eq!(tags(&repo, &home, &[]), ["v1.0", "v1.5", "v2.0", "v10.0"]);

    // A `-` prefix reverses it.
    git(&repo, &["config", "tag.sort", "-version:refname"]);
    assert_eq!(tags(&repo, &home, &[]), ["v10.0", "v2.0", "v1.5", "v1.0"]);

    // --sort on the CLI overrides the config back to lexical refname.
    assert_eq!(
        tags(&repo, &home, &["--sort=refname"]),
        ["v1.0", "v1.5", "v10.0", "v2.0"]
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
