//! `git pull` honors `pull.ff` (`true`/`false`/`only`) as the default
//! fast-forward policy, overriding `merge.ff`, with a CLI
//! `--ff`/`--no-ff`/`--ff-only` overriding the config. Regression guard for the
//! config being ignored (pull always fast-forwarding regardless of policy).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// An upstream at `c0`, cloned into `dn`. The upstream is left free to advance
/// afterwards so the clone can be brought into a fast-forward or a diverged
/// relationship on demand.
fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-pullcfg-{tag}-{}", std::process::id()));
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
    std::fs::write(up.join("f"), "a\n").unwrap();
    git(&up, &["add", "f"]);
    git(&up, &["commit", "-q", "-m", "c0"]);

    let dn = root.join("dn");
    git(&root, &["clone", "-q", up.to_str().unwrap(), dn.to_str().unwrap()]);
    git(&dn, &["config", "user.email", "t@e.x"]);
    git(&dn, &["config", "user.name", "t"]);
    (up, dn, home)
}

/// Advance the upstream by one commit so the clone can fast-forward onto it.
fn advance_upstream(up: &Path) {
    let mut content = std::fs::read_to_string(up.join("f")).unwrap();
    content.push_str("b\n");
    std::fs::write(up.join("f"), content).unwrap();
    git(up, &["add", "f"]);
    git(up, &["commit", "-q", "-m", "c1"]);
}

/// Add a local-only commit to the clone so a later upstream advance diverges.
fn local_commit(dn: &Path) {
    std::fs::write(dn.join("local"), "x\n").unwrap();
    git(dn, &["add", "local"]);
    git(dn, &["commit", "-q", "-m", "loc"]);
}

fn pull(repo: &Path, home: &Path, args: &[&str]) -> Output {
    let mut a = vec!["pull"];
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

fn rev(repo: &Path, spec: &str) -> String {
    git_out(repo, &["rev-parse", spec])
}

/// Number of parents of `HEAD` (1 for a linear commit, 2 for a merge commit).
fn head_parents(repo: &Path) -> usize {
    let line = git_out(repo, &["rev-list", "--parents", "-n", "1", "HEAD"]);
    // Format: "<commit> <parent1> [<parent2> ...]".
    line.split_whitespace().count() - 1
}

#[test]
fn pull_ff_only_config_allows_fast_forward() {
    let (up, dn, home) = fixture("ffonly-ff");
    advance_upstream(&up);
    git(&dn, &["config", "pull.ff", "only"]);

    let out = pull(&dn, &home, &["origin", "main"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    // A clean fast-forward: HEAD is now the upstream tip, no merge commit.
    assert_eq!(rev(&dn, "HEAD"), rev(&dn, "refs/remotes/origin/main"));
    assert_eq!(head_parents(&dn), 1);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Fast-forward"), "stdout: {stdout}");

    let _ = std::fs::remove_dir_all(dn.parent().unwrap());
}

#[test]
fn pull_ff_only_config_refuses_diverged() {
    let (up, dn, home) = fixture("ffonly-nonff");
    local_commit(&dn);
    advance_upstream(&up);
    git(&dn, &["config", "pull.ff", "only"]);

    let before = rev(&dn, "HEAD");
    let out = pull(&dn, &home, &["origin", "main"]);
    assert!(!out.status.success(), "pull.ff=only must refuse a diverged upstream");
    assert_eq!(out.status.code(), Some(128), "git aborts a non-ff --ff-only with exit 128");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Not possible to fast-forward, aborting."),
        "stderr: {stderr}"
    );
    // The refusal is before any ref/worktree mutation: HEAD is untouched.
    assert_eq!(rev(&dn, "HEAD"), before);

    let _ = std::fs::remove_dir_all(dn.parent().unwrap());
}

#[test]
fn pull_ff_false_config_creates_merge_commit() {
    let (up, dn, home) = fixture("fffalse");
    advance_upstream(&up);
    git(&dn, &["config", "pull.ff", "false"]);

    let out = pull(&dn, &home, &["origin", "main"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    // pull.ff=false forces a merge commit even over a fast-forwardable history.
    assert_eq!(head_parents(&dn), 2, "pull.ff=false must record a two-parent merge");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Merge made by the 'ort' strategy."), "stdout: {stdout}");

    let _ = std::fs::remove_dir_all(dn.parent().unwrap());
}

#[test]
fn pull_cli_ff_only_overrides_config() {
    let (up, dn, home) = fixture("cli-ffonly");
    local_commit(&dn);
    advance_upstream(&up);
    // Config would allow a (merge) integration; the CLI flag forces ff-only.
    git(&dn, &["config", "pull.ff", "true"]);

    let before = rev(&dn, "HEAD");
    let out = pull(&dn, &home, &["--ff-only", "origin", "main"]);
    assert!(!out.status.success(), "--ff-only must override pull.ff=true and refuse");
    assert_eq!(out.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Not possible to fast-forward, aborting."),
        "stderr: {stderr}"
    );
    assert_eq!(rev(&dn, "HEAD"), before);

    let _ = std::fs::remove_dir_all(dn.parent().unwrap());
}

#[test]
fn pull_cli_no_ff_overrides_ff_only_config() {
    let (up, dn, home) = fixture("cli-noff");
    advance_upstream(&up);
    // Config would refuse a non-ff; the CLI --no-ff forces a merge commit over a
    // fast-forwardable history — which pull.ff=only would otherwise fast-forward.
    git(&dn, &["config", "pull.ff", "only"]);

    let out = pull(&dn, &home, &["--no-ff", "origin", "main"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(head_parents(&dn), 2, "--no-ff must record a two-parent merge");

    let _ = std::fs::remove_dir_all(dn.parent().unwrap());
}
