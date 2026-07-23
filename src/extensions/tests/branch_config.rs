//! `git branch` honors the multi-valued `branch.sort` config as the default sort
//! order for the branch listing, with `--sort=<key>` on the command line adding
//! more-significant sort levels on top. git validates every field name (config or
//! CLI) at read time, dying with its exact `unknown/malformed field name` fatal
//! (exit 128). These are regression guards that zvcs matches git byte-for-byte
//! for the config default, the CLI override, and the invalid-value path.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Run the system git in `dir`, asserting success (used only for fixture setup).
fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .unwrap()
            .success(),
        "git {args:?} failed"
    );
}

/// Commit an empty commit with a fixed author/committer date so sort order by
/// date is deterministic across machines and runs.
fn commit_at(repo: &Path, msg: &str, date: &str) {
    assert!(
        Command::new("git")
            .args(["commit", "-q", "--allow-empty", "-m", msg])
            .current_dir(repo)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date)
            .status()
            .unwrap()
            .success(),
        "dated commit {msg:?} failed"
    );
}

/// A repo whose branches make every studied sort key observable:
///
/// | branch | commit date | refname order |
/// |--------|-------------|---------------|
/// | zebra  | 2020-01-01  | last          |
/// | apple  | 2022-06-01  | first         |
/// | mango  | 2021-03-01  | third         |
/// | main*  | 2021-03-01  | second        |
///
/// refname order (apple, main, mango, zebra) differs from committerdate order
/// (zebra, {main,mango}, apple), and `main`/`mango` tie on date so the implicit
/// refname tie-break is exercised too.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-branchcfg-{tag}-{}", std::process::id()));
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

    commit_at(&repo, "c0", "2020-01-01T00:00:00 +0000");
    git(&repo, &["branch", "zebra"]);
    commit_at(&repo, "c1", "2022-06-01T00:00:00 +0000");
    git(&repo, &["branch", "apple"]);
    commit_at(&repo, "c2", "2021-03-01T00:00:00 +0000");
    git(&repo, &["branch", "mango"]);
    (repo, home)
}

/// Run `git branch [extra]` under an isolated, deterministic environment. `bin`
/// is either the zvcs binary or system `git`, invoked with byte-identical env so
/// their listings are directly comparable.
fn run_branch(bin: &str, repo: &Path, home: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["branch"];
    args.extend_from_slice(extra);
    Command::new(bin)
        .args(&args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("ZVCS_HOME", home)
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .unwrap()
}

fn zvcs(repo: &Path, home: &Path, extra: &[&str]) -> Output {
    run_branch(BIN, repo, home, extra)
}

fn real(repo: &Path, home: &Path, extra: &[&str]) -> Output {
    run_branch("git", repo, home, extra)
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn branch_sort_keys_match_git() {
    let (repo, home) = fixture("keys");

    // Every backed key: refname (also the default), version:refname, dates in
    // both directions, and objectname. Each must equal git's listing exactly,
    // including the `*` current-branch marker landing at its sorted position.
    for key in [
        "refname",
        "-refname",
        "committerdate",
        "-committerdate",
        "authordate",
        "creatordate",
        "version:refname",
        "v:refname",
        "objectname",
    ] {
        let flag = format!("--sort={key}");
        let z = zvcs(&repo, &home, &[&flag]);
        let g = real(&repo, &home, &[&flag]);
        assert!(
            z.status.success(),
            "zvcs --sort={key} failed: {}",
            String::from_utf8_lossy(&z.stderr)
        );
        assert_eq!(out(&g), out(&z), "--sort={key} must match git byte-for-byte");
    }

    // Separate-argument form (`--sort committerdate`) parses like git's.
    let z = zvcs(&repo, &home, &["--sort", "committerdate"]);
    let g = real(&repo, &home, &["--sort", "committerdate"]);
    assert_eq!(out(&g), out(&z), "`--sort committerdate` must match git");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn branch_sort_config_default_and_override() {
    let (repo, home) = fixture("config");

    // branch.sort supplies the default order for a plain `git branch`.
    git(&repo, &["config", "branch.sort", "committerdate"]);
    let z = zvcs(&repo, &home, &[]);
    let g = real(&repo, &home, &[]);
    assert_eq!(out(&g), out(&z), "branch.sort=committerdate default must match git");
    assert!(
        out(&z).starts_with("  zebra\n"),
        "committerdate default lists the oldest (zebra) first:\n{}",
        out(&z)
    );

    // A CLI `--sort` is more significant than the config, so it reorders the
    // listing while the config key remains a lower-priority tie-break.
    let z = zvcs(&repo, &home, &["--sort=refname"]);
    let g = real(&repo, &home, &["--sort=refname"]);
    assert_eq!(out(&g), out(&z), "--sort=refname over branch.sort must match git");
    assert!(
        out(&z).starts_with("  apple\n"),
        "refname override lists alphabetically first (apple):\n{}",
        out(&z)
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn branch_sort_invalid_is_fatal() {
    let (repo, home) = fixture("invalid");

    // An unknown field name: git's exact fatal and exit code.
    let z = zvcs(&repo, &home, &["--sort=bogus"]);
    assert_eq!(z.status.code(), Some(128), "invalid --sort exits 128");
    assert_eq!(
        String::from_utf8_lossy(&z.stderr),
        "fatal: unknown field name: bogus\n"
    );

    // An empty field name is git's `malformed field name` fatal.
    let z = zvcs(&repo, &home, &["--sort="]);
    assert_eq!(z.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&z.stderr),
        "fatal: malformed field name: \n"
    );

    // git validates branch.sort while reading config, so a bad config value is
    // fatal even when a valid `--sort` override is also present.
    git(&repo, &["config", "branch.sort", "bogus"]);
    let z = zvcs(&repo, &home, &["--sort=refname"]);
    assert_eq!(
        z.status.code(),
        Some(128),
        "invalid branch.sort is fatal regardless of --sort"
    );
    assert_eq!(
        String::from_utf8_lossy(&z.stderr),
        "fatal: unknown field name: bogus\n"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
