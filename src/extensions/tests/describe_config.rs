//! `git describe` honors NO configuration keys, verified against real git 2.55.0:
//! its `--config-for-completion` list (1005 keys) contains zero `describe.*`
//! entries, and empirically `describe.tags` / `describe.abbrev` are both ignored
//! (`git -c describe.tags=true describe` still shows annotated-only; `-c
//! describe.abbrev=16 describe` still shows the repo-default hash width). A
//! faithful reimplementation must therefore ALSO ignore them — porting either as a
//! default-for-`--tags` / default-for-`--abbrev` would DIVERGE from git.
//!
//! This is a divergence guard: it pins that setting `describe.tags` /
//! `describe.abbrev` in the repo config does not change zvcs output, and that the
//! `--tags` / `--abbrev` command-line flags (the real controls) still work. If a
//! future change starts honoring these keys, these tests fail and flag the
//! git-incompatibility before it ships.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

/// HEAD (`c2`) with a lightweight tag `light` one commit back (`c1`) and an
/// annotated tag `v1` two commits back (`c0`). This layout makes the two keys
/// observable: honoring `describe.tags` would flip the name from `v1-2-…` to
/// `light-1-…`; honoring `describe.abbrev` would change the `g<hash>` width.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-descfg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "alice@example.com"]);
    git(&repo, &["config", "user.name", "Alice"]);
    std::fs::write(repo.join("f"), "1\n").unwrap();
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-q", "-m", "c0"]);
    git(&repo, &["tag", "-a", "v1", "-m", "v1"]);
    std::fs::write(repo.join("f"), "2\n").unwrap();
    git(&repo, &["commit", "-q", "-am", "c1"]);
    git(&repo, &["tag", "light"]);
    std::fs::write(repo.join("f"), "3\n").unwrap();
    git(&repo, &["commit", "-q", "-am", "c2"]);
    (repo, home)
}

fn describe(repo: &Path, home: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["describe"];
    args.extend_from_slice(extra);
    Command::new(BIN)
        .args(&args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .output()
        .unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).trim_end().to_owned()
}

#[test]
fn describe_tags_config_is_ignored_like_git() {
    let (repo, home) = fixture("tags");

    // Default: annotated tags only -> the annotated `v1`, not the nearer `light`.
    let base = stdout(&describe(&repo, &home, &[]));
    assert!(base.starts_with("v1-2-g"), "default names the annotated tag:\n{base}");

    // git ignores describe.tags entirely, so the output must not change.
    git(&repo, &["config", "describe.tags", "true"]);
    let cfg = stdout(&describe(&repo, &home, &[]));
    assert_eq!(cfg, base, "describe.tags must be ignored (git does not honor it)");

    // The real control, `--tags`, still selects the nearer lightweight tag.
    let flag = stdout(&describe(&repo, &home, &["--tags"]));
    assert!(flag.starts_with("light-1-g"), "--tags selects the lightweight tag:\n{flag}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn describe_abbrev_config_is_ignored_like_git() {
    let (repo, home) = fixture("abbrev");

    // Default abbreviation width (7 in this small repo) under --tags.
    let base = stdout(&describe(&repo, &home, &["--tags"]));
    let hash = base.rsplit_once("-g").expect("long form has -g<hash>").1;
    assert_eq!(hash.len(), 7, "default abbrev is 7 hex digits:\n{base}");

    // describe.abbrev is not honored by git; the width must stay at the default.
    git(&repo, &["config", "describe.abbrev", "16"]);
    let cfg = stdout(&describe(&repo, &home, &["--tags"]));
    assert_eq!(cfg, base, "describe.abbrev must be ignored (git does not honor it)");

    // describe.abbrev=0 must NOT suppress the hash suffix (that is `--abbrev=0`'s
    // job, not the config's) — git keeps the full long form.
    git(&repo, &["config", "describe.abbrev", "0"]);
    let cfg0 = stdout(&describe(&repo, &home, &["--tags"]));
    assert_eq!(cfg0, base, "describe.abbrev=0 must be ignored, suffix retained");

    // The real control, `--abbrev`, still resizes the hash.
    let flag = stdout(&describe(&repo, &home, &["--tags", "--abbrev=16"]));
    let hash16 = flag.rsplit_once("-g").unwrap().1;
    assert_eq!(hash16.len(), 16, "--abbrev=16 widens the hash to 16 digits:\n{flag}");

    // And `--abbrev=0` (the flag, not the config) drops the suffix to the bare tag.
    let flag0 = stdout(&describe(&repo, &home, &["--tags", "--abbrev=0"]));
    assert_eq!(flag0, "light", "--abbrev=0 suppresses the suffix to the bare tag:\n{flag0}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
