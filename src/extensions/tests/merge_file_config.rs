//! `git merge-file` honors `merge.conflictStyle` as the default conflict-marker
//! style, with the `--diff3`/`--zdiff3` CLI flags still overriding it, and
//! rejects an unknown value the way git's `git_xmerge_config` does. Each style
//! test diffs zvcs's marker output against stock git byte-for-byte on identical
//! inputs; the validation tests compare the fatal `error:` line and exit code.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A repo with three files whose middle line conflicts three ways, plus an
/// isolated HOME so only the repo's `.git/config` is consulted.
fn setup(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-mfcfg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    let repo = repo.canonicalize().unwrap();
    let home = home.canonicalize().unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-q", "."])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success(),
        "git init failed"
    );
    std::fs::write(repo.join("base"), "a\nb\nc\n").unwrap();
    std::fs::write(repo.join("cur"), "a\nOURS\nc\n").unwrap();
    std::fs::write(repo.join("oth"), "a\nTHEIRS\nc\n").unwrap();
    (repo, home)
}

/// Set a repo-local config value with real git.
fn config(repo: &Path, key: &str, value: &str) {
    assert!(
        Command::new("git")
            .args(["config", key, value])
            .current_dir(repo)
            .status()
            .unwrap()
            .success(),
        "git config {key} failed"
    );
}

/// Run `bin merge-file <args>` in `repo` under the isolated environment.
fn run(bin: &str, repo: &Path, home: &Path, args: &[&str]) -> Output {
    let mut full = vec!["merge-file"];
    full.extend_from_slice(args);
    Command::new(bin)
        .args(&full)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .output()
        .unwrap()
}

/// Assert zvcs and stock git produce identical stdout and exit code for the
/// same `merge-file` invocation — the byte-for-byte marker-output guarantee.
fn assert_matches_git(repo: &Path, home: &Path, args: &[&str]) -> Output {
    let ours = run(BIN, repo, home, args);
    let theirs = run("git", repo, home, args);
    assert_eq!(
        ours.stdout, theirs.stdout,
        "stdout differs for args {args:?}\nzvcs: {:?}\ngit:  {:?}",
        String::from_utf8_lossy(&ours.stdout),
        String::from_utf8_lossy(&theirs.stdout),
    );
    assert_eq!(
        ours.status.code(),
        theirs.status.code(),
        "exit code differs for args {args:?}",
    );
    ours
}

fn cleanup(repo: &Path) {
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn default_is_merge_style() {
    let (repo, home) = setup("default");
    let out = assert_matches_git(&repo, &home, &["-p", "cur", "base", "oth"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("<<<<<<< cur"), "expected merge markers: {text}");
    assert!(!text.contains("|||||||"), "merge style has no base section: {text}");
    assert_eq!(out.status.code(), Some(1), "one conflict");
    cleanup(&repo);
}

#[test]
fn conflict_style_diff3_adds_base_section() {
    let (repo, home) = setup("diff3");
    config(&repo, "merge.conflictStyle", "diff3");
    let out = assert_matches_git(&repo, &home, &["-p", "cur", "base", "oth"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("||||||| base"), "diff3 must emit the base section: {text}");
    assert!(text.contains("b\n"), "the base line belongs in the base section: {text}");
    cleanup(&repo);
}

#[test]
fn conflict_style_zdiff3_matches_git() {
    let (repo, home) = setup("zdiff3");
    config(&repo, "merge.conflictStyle", "zdiff3");
    let out = assert_matches_git(&repo, &home, &["-p", "cur", "base", "oth"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("||||||| base"), "zdiff3 keeps the base section: {text}");
    cleanup(&repo);
}

#[test]
fn conflict_style_merge_value_is_plain_style() {
    let (repo, home) = setup("mergeval");
    config(&repo, "merge.conflictStyle", "merge");
    let out = assert_matches_git(&repo, &home, &["-p", "cur", "base", "oth"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(!text.contains("|||||||"), "merge value = no base section: {text}");
    cleanup(&repo);
}

#[test]
fn diff3_flag_overrides_merge_config() {
    // A CLI style flag wins over the config default, both in zvcs and git.
    let (repo, home) = setup("flagover");
    config(&repo, "merge.conflictStyle", "merge");
    let out = assert_matches_git(&repo, &home, &["-p", "--diff3", "cur", "base", "oth"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("||||||| base"), "--diff3 must override merge config: {text}");
    cleanup(&repo);
}

#[test]
fn zdiff3_flag_overrides_merge_config() {
    let (repo, home) = setup("zdiff3flag");
    config(&repo, "merge.conflictStyle", "merge");
    let out = assert_matches_git(&repo, &home, &["-p", "--zdiff3", "cur", "base", "oth"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("||||||| base"), "--zdiff3 overrides merge config: {text}");
    cleanup(&repo);
}

#[test]
fn lowercase_config_key_is_read() {
    // git config keys are case-insensitive; the lowercase spelling must select
    // diff3 exactly as the camelCase form does.
    let (repo, home) = setup("lowerkey");
    config(&repo, "merge.conflictstyle", "diff3");
    let out = assert_matches_git(&repo, &home, &["-p", "cur", "base", "oth"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("||||||| base"), "lowercase key must be honored: {text}");
    cleanup(&repo);
}

#[test]
fn unknown_value_is_fatal() {
    // git validates the value in its config callback and dies 128 before merging.
    let (repo, home) = setup("badval");
    config(&repo, "merge.conflictStyle", "bogus");
    let ours = run(BIN, &repo, &home, &["-p", "cur", "base", "oth"]);
    let theirs = run("git", &repo, &home, &["-p", "cur", "base", "oth"]);
    assert_eq!(ours.status.code(), Some(128), "unknown style must exit 128");
    assert_eq!(theirs.status.code(), Some(128), "git also exits 128");
    let our_err = String::from_utf8_lossy(&ours.stderr);
    let git_err = String::from_utf8_lossy(&theirs.stderr);
    let our_first = our_err.lines().next().unwrap_or_default();
    let git_first = git_err.lines().next().unwrap_or_default();
    assert_eq!(
        our_first, git_first,
        "the error: line must match git byte-for-byte\nzvcs: {our_err}\ngit:  {git_err}"
    );
    assert_eq!(
        our_first, "error: unknown style 'bogus' given for 'merge.conflictstyle'",
        "exact git wording"
    );
    assert!(
        our_err.contains("fatal: bad config variable 'merge.conflictstyle' in file"),
        "the fatal line names the config file: {our_err}"
    );
    cleanup(&repo);
}

#[test]
fn unknown_value_is_fatal_even_with_flag_override() {
    // Validation runs before option parsing, so --diff3 does not rescue a bad
    // config value — git dies 128 all the same.
    let (repo, home) = setup("badflag");
    config(&repo, "merge.conflictStyle", "Diff3"); // case-sensitive: rejected
    let ours = run(BIN, &repo, &home, &["-p", "--diff3", "cur", "base", "oth"]);
    let theirs = run("git", &repo, &home, &["-p", "--diff3", "cur", "base", "oth"]);
    assert_eq!(ours.status.code(), Some(128), "bad config is fatal despite --diff3");
    assert_eq!(theirs.status.code(), Some(128), "git agrees");
    let our_first = String::from_utf8_lossy(&ours.stderr).lines().next().unwrap_or_default().to_string();
    let git_first = String::from_utf8_lossy(&theirs.stderr).lines().next().unwrap_or_default().to_string();
    assert_eq!(our_first, git_first, "error line matches git");
    assert_eq!(our_first, "error: unknown style 'Diff3' given for 'merge.conflictstyle'");
    cleanup(&repo);
}

/// Append a repo-local multivar entry with real git.
fn config_add(repo: &Path, key: &str, value: &str) {
    assert!(
        Command::new("git")
            .args(["config", "--add", key, value])
            .current_dir(repo)
            .status()
            .unwrap()
            .success(),
        "git config --add {key} failed"
    );
}

#[test]
fn later_valid_value_wins() {
    // Multiple values: the last valid one is the effective style, matching git's
    // last-wins config precedence. `merge` first, then `diff3`; diff3 must win.
    let (repo, home) = setup("lastwins");
    config_add(&repo, "merge.conflictStyle", "merge");
    config_add(&repo, "merge.conflictStyle", "diff3");
    let out = assert_matches_git(&repo, &home, &["-p", "cur", "base", "oth"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("||||||| base"), "last value diff3 must win: {text}");
    cleanup(&repo);
}

#[test]
fn earlier_invalid_value_is_fatal() {
    // git validates every occurrence as config is parsed, so a bad value that a
    // later valid one would override is still fatal.
    let (repo, home) = setup("badfirst");
    config_add(&repo, "merge.conflictStyle", "bogus");
    config_add(&repo, "merge.conflictStyle", "diff3");
    let ours = run(BIN, &repo, &home, &["-p", "cur", "base", "oth"]);
    let theirs = run("git", &repo, &home, &["-p", "cur", "base", "oth"]);
    assert_eq!(ours.status.code(), Some(128), "earlier bad value is fatal");
    assert_eq!(theirs.status.code(), Some(128), "git agrees");
    let our_first = String::from_utf8_lossy(&ours.stderr).lines().next().unwrap_or_default().to_string();
    assert_eq!(our_first, "error: unknown style 'bogus' given for 'merge.conflictstyle'");
    cleanup(&repo);
}
