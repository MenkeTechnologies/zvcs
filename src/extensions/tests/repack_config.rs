//! `git repack` honors `repack.updateServerInfo` as the default for the closing
//! `update-server-info` step.
//!
//! git keeps a single `run_update_server_info`, seeded from the config (default
//! true) and cleared by `-n` (an `OPT_NEGBIT`), then runs `update_server_info`
//! only when it survives. So the config's one observable effect is whether the
//! run refreshes `.git/objects/info/packs`, and `-n` always wins over a config
//! that enables it. Bitmaps, delta-base-offset, kept-object and cruft repack
//! keys tune work this delta-free, cruft-free writer never performs, so they are
//! deliberately not read and are not exercised here.
//!
//! Every case is checked against the system `git` (2.55.0) run with a
//! byte-identical environment; the assertion is the presence or absence of
//! `objects/info/packs`, which is the signal `repack.updateServerInfo` controls.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// The file git's `update-server-info` refreshes; its presence after a repack is
/// the observable `repack.updateServerInfo` gates.
const INFO_PACKS: &str = ".git/objects/info/packs";

/// Run a system-`git` command in `dir`, asserting success. Used only to build
/// the fixture and to write `.git/config`, never as the behavior under test.
fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

/// A one-commit repository plus an isolated, empty `HOME`, so no ambient global
/// `repack.*` config leaks into the run.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-rpcfg-{tag}-{}", std::process::id()));
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
    std::fs::write(repo.join("f"), "hello\n").unwrap();
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-q", "-m", "c0"]);
    (repo, home)
}

/// Run `git repack [extra]` under a deterministic, isolated environment. `bin` is
/// either the zvcs binary or the system `git`, run with byte-identical env so
/// their effects are directly comparable.
fn run_repack(bin: &str, repo: &Path, home: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["repack"];
    args.extend_from_slice(extra);
    Command::new(bin)
        .args(&args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("ZVCS_HOME", home)
        .env("LC_ALL", "C")
        .output()
        .unwrap()
}

fn zvcs(repo: &Path, home: &Path, extra: &[&str]) -> Output {
    run_repack(BIN, repo, home, extra)
}

fn real(repo: &Path, home: &Path, extra: &[&str]) -> Output {
    run_repack("git", repo, home, extra)
}

fn info_packs_present(repo: &Path) -> bool {
    repo.join(INFO_PACKS).exists()
}

/// Remove any `objects/info/packs` left by a prior step so the next repack's
/// effect is observed in isolation.
fn clear_info_packs(repo: &Path) {
    let _ = std::fs::remove_file(repo.join(INFO_PACKS));
}

#[test]
fn repack_update_server_info_default_writes_info_packs() {
    // With the config unset, `repack -a -d` refreshes objects/info/packs, and the
    // port agrees with git that it does.
    let (repo, home) = fixture("default");

    let z = zvcs(&repo, &home, &["-a", "-d", "-q"]);
    assert!(z.status.success(), "repack must succeed:\n{}", String::from_utf8_lossy(&z.stderr));
    assert!(info_packs_present(&repo), "default repack must write objects/info/packs");

    clear_info_packs(&repo);
    let g = real(&repo, &home, &["-a", "-d", "-q"]);
    assert!(g.status.success(), "sanity: git repack succeeds");
    assert!(info_packs_present(&repo), "sanity: git default repack writes objects/info/packs");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn repack_update_server_info_false_suppresses_info_packs() {
    // `repack.updateServerInfo=false` skips the closing update-server-info, so no
    // objects/info/packs appears — matching git.
    let (repo, home) = fixture("false");
    git(&repo, &["config", "repack.updateServerInfo", "false"]);

    let z = zvcs(&repo, &home, &["-a", "-d", "-q"]);
    assert!(z.status.success(), "repack must still succeed:\n{}", String::from_utf8_lossy(&z.stderr));
    assert!(!info_packs_present(&repo), "false config must suppress objects/info/packs");

    clear_info_packs(&repo);
    let g = real(&repo, &home, &["-a", "-d", "-q"]);
    assert!(g.status.success());
    assert!(!info_packs_present(&repo), "sanity: git also suppresses it under false");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn repack_update_server_info_dash_n_overrides_true_config() {
    // git's `run_update_server_info` is an `OPT_NEGBIT` that `-n` clears, so `-n`
    // wins over a `repack.updateServerInfo=true` config: no objects/info/packs.
    let (repo, home) = fixture("dashn");
    git(&repo, &["config", "repack.updateServerInfo", "true"]);

    let z = zvcs(&repo, &home, &["-a", "-d", "-n", "-q"]);
    assert!(z.status.success(), "repack must succeed:\n{}", String::from_utf8_lossy(&z.stderr));
    assert!(!info_packs_present(&repo), "-n must win over a config that enables server info");

    clear_info_packs(&repo);
    let g = real(&repo, &home, &["-a", "-d", "-n", "-q"]);
    assert!(g.status.success());
    assert!(!info_packs_present(&repo), "sanity: git's -n also wins over the config");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn repack_update_server_info_false_suppresses_in_nothing_new_path() {
    // git runs update-server-info unconditionally at the end, even when there was
    // nothing new to pack; `repack.updateServerInfo=false` suppresses it there
    // too. The repo is packed first so a plain incremental `repack` finds nothing
    // to do.
    let (repo, home) = fixture("nothingnew");
    let first = zvcs(&repo, &home, &["-q"]);
    assert!(first.status.success(), "priming repack must succeed");
    clear_info_packs(&repo);
    git(&repo, &["config", "repack.updateServerInfo", "false"]);

    let z = zvcs(&repo, &home, &["-q"]);
    assert!(z.status.success(), "nothing-new repack must succeed:\n{}", String::from_utf8_lossy(&z.stderr));
    assert!(!info_packs_present(&repo), "false config must suppress server info in the nothing-new path");

    // git behaves the same: prime, clear, set false, repack — still nothing.
    let (repo2, home2) = fixture("nothingnew-git");
    assert!(real(&repo2, &home2, &["-q"]).status.success());
    clear_info_packs(&repo2);
    git(&repo2, &["config", "repack.updateServerInfo", "false"]);
    assert!(real(&repo2, &home2, &["-q"]).status.success());
    assert!(!info_packs_present(&repo2), "sanity: git suppresses it in its nothing-new path");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    let _ = std::fs::remove_dir_all(repo2.parent().unwrap());
}

#[test]
fn repack_update_server_info_falsey_bool_forms_match_git() {
    // The key is an `OPT_BOOL`/`git_config_bool`, so `off`, `no` and `0` are all
    // false. Each must suppress objects/info/packs exactly as git does.
    for value in ["off", "no", "0"] {
        let (repo, home) = fixture(&format!("bool-{value}"));
        git(&repo, &["config", "repack.updateServerInfo", value]);

        let z = zvcs(&repo, &home, &["-a", "-d", "-q"]);
        assert!(z.status.success(), "repack.updateServerInfo={value} must be accepted");
        let z_present = info_packs_present(&repo);

        clear_info_packs(&repo);
        let g = real(&repo, &home, &["-a", "-d", "-q"]);
        assert!(g.status.success());
        let g_present = info_packs_present(&repo);

        assert!(!z_present, "repack.updateServerInfo={value} must suppress objects/info/packs");
        assert_eq!(z_present, g_present, "repack.updateServerInfo={value} must match git");

        let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    }
}
