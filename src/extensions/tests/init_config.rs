//! `git init` resolves the initial branch with git's exact precedence:
//! `-b`/`--initial-branch` on the command line, else the `init.defaultBranch`
//! config value, else the compiled-in default `master`. Each case asserts the
//! resulting `.git/HEAD` byte-for-byte and cross-checks it against stock git run
//! under an identical HOME/env, guarding against the gix `main` fallback leaking
//! through and against the config default being ignored.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Create an isolated root with a `home/` (optionally carrying a `.gitconfig`
/// with `init.defaultBranch = <branch>`) plus empty `zvcs/` and `real/` dirs to
/// init into.
fn fixture(tag: &str, default_branch: Option<&str>) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-initcfg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();

    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    if let Some(branch) = default_branch {
        std::fs::write(
            home.join(".gitconfig"),
            format!("[init]\n\tdefaultBranch = {branch}\n"),
        )
        .unwrap();
    }

    let zvcs = root.join("zvcs");
    let real = root.join("real");
    std::fs::create_dir_all(&zvcs).unwrap();
    std::fs::create_dir_all(&real).unwrap();
    (home, zvcs, real)
}

/// Run `init` (with any extra flags) under the given HOME + `GIT_CONFIG_NOSYSTEM`
/// and return the resulting `.git/HEAD` contents verbatim.
fn init_head(bin: &str, dir: &Path, home: &Path, extra: &[&str]) -> String {
    let mut args = vec!["init", "-q"];
    args.extend_from_slice(extra);
    let ok = Command::new(bin)
        .args(&args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap()
        .success();
    assert!(ok, "{bin} {args:?} failed in {dir:?}");
    std::fs::read_to_string(dir.join(".git/HEAD")).unwrap()
}

/// Assert zvcs and stock git produce the same `.git/HEAD`, and that it names the
/// expected branch.
fn assert_head(tag: &str, default_branch: Option<&str>, extra: &[&str], want_branch: &str) {
    let (home, zvcs, real) = fixture(tag, default_branch);
    let zvcs_head = init_head(BIN, &zvcs, &home, extra);
    let real_head = init_head("git", &real, &home, extra);
    let expected = format!("ref: refs/heads/{want_branch}\n");
    assert_eq!(zvcs_head, real_head, "zvcs HEAD differs from stock git ({tag})");
    assert_eq!(zvcs_head, expected, "zvcs HEAD not the expected branch ({tag})");
    let _ = std::fs::remove_dir_all(zvcs.parent().unwrap());
}

#[test]
fn init_default_branch_config_sets_head() {
    // init.defaultBranch, no CLI flag -> HEAD points at the configured branch.
    assert_head("cfg", Some("trunk"), &[], "trunk");
}

#[test]
fn init_b_flag_overrides_default_branch_config() {
    // -b <name> wins over init.defaultBranch.
    assert_head("bflag", Some("trunk"), &["-b", "feature"], "feature");
}

#[test]
fn init_initial_branch_eq_overrides_default_branch_config() {
    // --initial-branch=<name> wins over init.defaultBranch.
    assert_head("eqflag", Some("trunk"), &["--initial-branch=dev"], "dev");
}

#[test]
fn init_without_config_or_flag_defaults_to_master() {
    // No init.defaultBranch and no flag -> git's compiled default `master`
    // (must NOT leak gix's `main` fallback).
    assert_head("nocfg", None, &[], "master");
}

#[test]
fn init_no_config_flag_still_wins() {
    // With no config, an explicit -b is still honored verbatim.
    assert_head("nocfg-bflag", None, &["-b", "release"], "release");
}
