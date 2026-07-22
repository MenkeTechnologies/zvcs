//! `git switch <remote-only-name>` DWIM honors `checkout.guess`: on by default,
//! disabled by `checkout.guess=false`, with `--guess`/`--no-guess` overriding.
//! Regression guard for the config being ignored (DWIM always on).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

/// An upstream with a `feature` branch, cloned into `dn` so `origin/feature`
/// exists but no local `feature` — the DWIM trigger.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-switchcfg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let up = root.join("up");
    std::fs::create_dir_all(&up).unwrap();
    git(&up, &["init", "-q", "-b", "main"]);
    git(&up, &["config", "user.email", "t@e.x"]);
    git(&up, &["config", "user.name", "t"]);
    std::fs::write(up.join("f"), "x\n").unwrap();
    git(&up, &["add", "f"]);
    git(&up, &["commit", "-q", "-m", "c0"]);
    git(&up, &["branch", "feature"]);

    let dn = root.join("dn");
    git(&root, &["clone", "-q", up.to_str().unwrap(), dn.to_str().unwrap()]);
    git(&dn, &["config", "user.email", "t@e.x"]);
    git(&dn, &["config", "user.name", "t"]);
    (dn, home)
}

fn switch(repo: &Path, home: &Path, args: &[&str]) -> Output {
    let mut a = vec!["switch"];
    a.extend_from_slice(args);
    Command::new(BIN)
        .args(&a)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .output()
        .unwrap()
}

fn current_branch(repo: &Path) -> String {
    let out = Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(repo)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn config_get(repo: &Path, key: &str) -> String {
    let out = Command::new("git")
        .args(["config", "--get", key])
        .current_dir(repo)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn branch_auto_setup_rebase() {
    let (repo, home) = fixture("autorebase");

    // "always": a new branch tracking a remote gets branch.<name>.rebase=true.
    git(&repo, &["config", "branch.autoSetupRebase", "always"]);
    let out = switch(&repo, &home, &["-c", "feat", "origin/feature"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(config_get(&repo, "branch.feat.rebase"), "true");

    // "never" (the default): no rebase key is written.
    git(&repo, &["config", "branch.autoSetupRebase", "never"]);
    let out = switch(&repo, &home, &["-c", "feat2", "origin/feature"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(config_get(&repo, "branch.feat2.rebase"), "");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn checkout_guess_config_and_override() {
    let (repo, home) = fixture("guess");

    // Default: DWIM creates a local `feature` tracking origin/feature.
    let out = switch(&repo, &home, &["feature"]);
    assert!(out.status.success(), "default DWIM failed: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(current_branch(&repo), "feature");

    // Reset, then disable DWIM: the bare name no longer resolves.
    git(&repo, &["switch", "-q", "main"]);
    git(&repo, &["branch", "-q", "-D", "feature"]);
    git(&repo, &["config", "checkout.guess", "false"]);
    let out = switch(&repo, &home, &["feature"]);
    assert!(!out.status.success(), "checkout.guess=false must disable DWIM");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("invalid reference"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(current_branch(&repo), "main");

    // --guess overrides the config back on.
    let out = switch(&repo, &home, &["--guess", "feature"]);
    assert!(out.status.success(), "--guess must override config: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(current_branch(&repo), "feature");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
