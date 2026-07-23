//! `git add` honors `add.ignoreErrors` (alias `add.ignore-errors`) as the
//! default for `--ignore-errors`: when adding several paths and one cannot be
//! read, a true value skips it and continues (staging the rest, exit 1) instead
//! of aborting before touching the index (exit 128). The command line
//! `--ignore-errors`/`--no-ignore-errors` overrides the config, matching git's
//! config-then-CLI precedence.
//!
//! These tests pin zvcs to stock git byte-for-byte — stdout, stderr, exit code,
//! and the resulting staged index — across the config default, a CLI `--no-`
//! override, a CLI `--` override of a false config, and the hyphenated alias.
//!
//! The unreadable path is produced with `chmod 000`. Under an effective root uid
//! (common in headless CI) permission bits are bypassed, so the read never
//! fails; each test detects that up front and returns without asserting rather
//! than exercising a code path that cannot fire on the platform.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

/// True when a `chmod 000` file is genuinely unreadable here. Returns false when
/// the effective uid bypasses permission bits (root), so the read-error path
/// under test cannot be provoked and the caller should skip.
fn unreadable_files_supported() -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let p = std::env::temp_dir().join(format!("zvcs-addcfg-probe-{}", std::process::id()));
        let _ = std::fs::remove_file(&p);
        if std::fs::write(&p, b"x").is_err() {
            return false;
        }
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o000));
        let readable = std::fs::read(&p).is_ok();
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644));
        let _ = std::fs::remove_file(&p);
        !readable
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// A fresh repo (+ isolated HOME) seeded with two readable files and one that
/// `chmod 000` makes unreadable, plus any `add.*` config keys. Each add run gets
/// its own fixture so the mutated index is compared from an identical start.
fn fixture(tag: &str, config: &[(&str, &str)]) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-addcfg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    for (k, v) in config {
        git(&repo, &["config", k, v]);
    }
    std::fs::write(repo.join("good1"), b"aaa\n").unwrap();
    std::fs::write(repo.join("good2"), b"ccc\n").unwrap();
    let bad = repo.join("bad");
    std::fs::write(&bad, b"bbb\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o000)).unwrap();
    }
    (root, repo, home)
}

/// Restore the `chmod 000` bit (a prior run may have relaxed it), run
/// `<bin> add <extra> .` under a byte-identical isolated environment, and return
/// the process output plus the sorted list of staged paths (read back with stock
/// git so the index is compared by content, not by which binary wrote it).
fn run_add(bin: &str, repo: &Path, home: &Path, extra: &[&str]) -> (Output, Vec<String>) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(repo.join("bad"), std::fs::Permissions::from_mode(0o000)).unwrap();
    }
    let mut args = vec!["add"];
    args.extend_from_slice(extra);
    args.push(".");
    let out = Command::new(bin)
        .args(&args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("ZVCS_HOME", home)
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .unwrap();
    let staged = String::from_utf8(
        Command::new("git")
            .args(["ls-files"])
            .current_dir(repo)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let mut list: Vec<String> = staged.lines().map(str::to_owned).collect();
    list.sort();
    (out, list)
}

/// Reset the index to empty, then assert zvcs and stock git agree on stdout,
/// stderr, exit code, and the staged set for the same config + command line.
fn assert_match(repo: &Path, home: &Path, extra: &[&str], what: &str) {
    // Independent fresh index for each binary so neither sees the other's stage.
    git(repo, &["read-tree", "--empty"]);
    let (z, zs) = run_add(BIN, repo, home, extra);
    git(repo, &["read-tree", "--empty"]);
    let (g, gs) = run_add("git", repo, home, extra);
    assert_eq!(z.status.code(), g.status.code(), "{what}: exit code");
    assert_eq!(
        String::from_utf8_lossy(&z.stdout),
        String::from_utf8_lossy(&g.stdout),
        "{what}: stdout"
    );
    assert_eq!(
        String::from_utf8_lossy(&z.stderr),
        String::from_utf8_lossy(&g.stderr),
        "{what}: stderr"
    );
    assert_eq!(zs, gs, "{what}: staged index");
}

#[test]
fn add_ignore_errors_config_true_continues_like_git() {
    if !unreadable_files_supported() {
        eprintln!("skip: chmod 000 is readable here (root); add.ignoreErrors path can't fire");
        return;
    }
    let (root, repo, home) = fixture("cfgtrue", &[("add.ignoreErrors", "true")]);

    // Default from config: the unreadable file is reported and skipped, the two
    // readable files stage, exit 1 — no `--ignore-errors` on the command line.
    assert_match(&repo, &home, &[], "add.ignoreErrors=true default");
    git(&repo, &["read-tree", "--empty"]);
    let (z, zs) = run_add(BIN, &repo, &home, &[]);
    assert_eq!(z.status.code(), Some(1), "continues, exit 1");
    assert_eq!(zs, vec!["good1".to_string(), "good2".to_string()], "readable files staged");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn add_no_ignore_errors_cli_overrides_true_config() {
    if !unreadable_files_supported() {
        eprintln!("skip: chmod 000 is readable here (root)");
        return;
    }
    let (root, repo, home) = fixture("clioverride", &[("add.ignoreErrors", "true")]);

    // `--no-ignore-errors` on the command line beats the true config: git aborts
    // before touching the index (exit 128), staging nothing.
    assert_match(&repo, &home, &["--no-ignore-errors"], "true config + --no-ignore-errors");
    git(&repo, &["read-tree", "--empty"]);
    let (z, zs) = run_add(BIN, &repo, &home, &["--no-ignore-errors"]);
    assert_eq!(z.status.code(), Some(128), "aborts, exit 128");
    assert!(zs.is_empty(), "nothing staged after abort");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn add_ignore_errors_cli_overrides_false_config() {
    if !unreadable_files_supported() {
        eprintln!("skip: chmod 000 is readable here (root)");
        return;
    }
    // add.ignoreErrors=false is git's default; also assert the plain default
    // (no config, no flag) aborts, then that `--ignore-errors` overrides it.
    let (root, repo, home) = fixture("clifalse", &[("add.ignoreErrors", "false")]);

    assert_match(&repo, &home, &[], "add.ignoreErrors=false default aborts");
    assert_match(&repo, &home, &["--ignore-errors"], "false config + --ignore-errors");
    git(&repo, &["read-tree", "--empty"]);
    let (z, zs) = run_add(BIN, &repo, &home, &["--ignore-errors"]);
    assert_eq!(z.status.code(), Some(1), "override continues, exit 1");
    assert_eq!(zs, vec!["good1".to_string(), "good2".to_string()], "readable files staged");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn add_ignore_errors_hyphen_alias_matches_git() {
    if !unreadable_files_supported() {
        eprintln!("skip: chmod 000 is readable here (root)");
        return;
    }
    // The hyphenated alias `add.ignore-errors` drives the same default as the
    // canonical camelCase key.
    let (root, repo, home) = fixture("alias", &[("add.ignore-errors", "true")]);

    assert_match(&repo, &home, &[], "add.ignore-errors alias default");
    git(&repo, &["read-tree", "--empty"]);
    let (z, _) = run_add(BIN, &repo, &home, &[]);
    assert_eq!(z.status.code(), Some(1), "alias enables continue, exit 1");

    let _ = std::fs::remove_dir_all(&root);
}
