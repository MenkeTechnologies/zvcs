//! `git merge` honors `merge.ff` as the fast-forward default, with the CLI
//! (`--ff-only`/`--ff`/`--no-ff`) still overriding. Regression guard for the
//! config being ignored (always fast-forwarding a linear history).

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new(BIN).args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

/// A repo where `feat` is strictly ahead of the integration branch `branch` (a
/// fast-forwardable merge), with `branch` checked out.
fn fixture_on(tag: &str, branch: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-mergecfg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", branch]);
    git(&repo, &["config", "user.email", "t@e.x"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("f"), "base\n").unwrap();
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    git(&repo, &["checkout", "-q", "-b", "feat"]);
    std::fs::write(repo.join("f"), "base\nmore\n").unwrap();
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-q", "-m", "feat"]);
    git(&repo, &["checkout", "-q", branch]);
    (repo, home)
}

/// A repo whose integration branch is `main`.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    fixture_on(tag, "main")
}

/// Subject line (`%s`) of `HEAD`'s commit.
fn subject(repo: &Path) -> String {
    let out = Command::new(BIN)
        .args(["log", "-1", "--format=%s"])
        .current_dir(repo)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

fn run(repo: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .output()
        .unwrap()
}

/// Number of parents of HEAD (2 = a merge commit, 1 = fast-forward tip).
fn head_parents(repo: &Path) -> usize {
    let out = Command::new(BIN)
        .args(["rev-list", "--parents", "-n", "1", "HEAD"])
        .current_dir(repo)
        .output()
        .unwrap();
    // Line is "<commit> <parent1> [<parent2> ...]" — parents = words - 1.
    String::from_utf8_lossy(&out.stdout).split_whitespace().count() - 1
}

#[test]
fn merge_ff_false_forces_a_merge_commit() {
    let (repo, home) = fixture("noff");
    git(&repo, &["config", "merge.ff", "false"]);
    let out = run(&repo, &home, &["merge", "feat", "-m", "merge feat"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(head_parents(&repo), 2, "merge.ff=false must create a merge commit");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn default_fast_forwards() {
    let (repo, home) = fixture("ff");
    let out = run(&repo, &home, &["merge", "feat"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(head_parents(&repo), 1, "a linear history should fast-forward by default");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn ff_only_flag_overrides_merge_ff_false() {
    let (repo, home) = fixture("ffonly");
    git(&repo, &["config", "merge.ff", "false"]);
    let out = run(&repo, &home, &["merge", "--ff-only", "feat"]);
    assert!(out.status.success(), "--ff-only should still fast-forward: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(head_parents(&repo), 1, "--ff-only must override merge.ff=false");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

// `merge.suppressDest`: the default merge message's ` into <branch>` title
// suffix is dropped when the current branch matches one of the (multi-valued,
// glob) patterns. Unset, the list defaults to `main`/`master`.

#[test]
fn default_suppresses_into_main() {
    let (repo, home) = fixture_on("sd-defmain", "main");
    let out = run(&repo, &home, &["merge", "--no-ff", "feat"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(subject(&repo), "Merge branch 'feat'", "main is suppressed by default");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn unmatched_branch_keeps_into_suffix() {
    let (repo, home) = fixture_on("sd-dev", "dev");
    let out = run(&repo, &home, &["merge", "--no-ff", "feat"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(subject(&repo), "Merge branch 'feat' into dev", "dev is not a default-suppressed branch");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn suppress_dest_matches_current_branch() {
    let (repo, home) = fixture_on("sd-match", "dev");
    git(&repo, &["config", "merge.suppressDest", "dev"]);
    let out = run(&repo, &home, &["merge", "--no-ff", "feat"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(subject(&repo), "Merge branch 'feat'", "merge.suppressDest=dev must suppress ' into dev'");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn suppress_dest_replaces_builtin_default() {
    // Setting the variable replaces the built-in main/master default, so main
    // is no longer suppressed.
    let (repo, home) = fixture_on("sd-repl", "main");
    git(&repo, &["config", "merge.suppressDest", "dev"]);
    let out = run(&repo, &home, &["merge", "--no-ff", "feat"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(subject(&repo), "Merge branch 'feat' into main", "an explicit list drops the main/master default");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn suppress_dest_glob_matches() {
    let (repo, home) = fixture_on("sd-glob", "release");
    git(&repo, &["config", "merge.suppressDest", "re*"]);
    let out = run(&repo, &home, &["merge", "--no-ff", "feat"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(subject(&repo), "Merge branch 'feat'", "the glob re* must match release");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn suppress_dest_is_case_sensitive() {
    let (repo, home) = fixture_on("sd-case", "release");
    git(&repo, &["config", "merge.suppressDest", "RE*"]);
    let out = run(&repo, &home, &["merge", "--no-ff", "feat"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(subject(&repo), "Merge branch 'feat' into release", "wildmatch here is case-sensitive");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn suppress_dest_empty_value_clears_the_list() {
    // An empty value wipes the accumulated patterns (including the default);
    // the trailing `xyz` does not match main, so the suffix survives.
    let (repo, home) = fixture_on("sd-clear", "main");
    git(&repo, &["config", "--add", "merge.suppressDest", ""]);
    git(&repo, &["config", "--add", "merge.suppressDest", "xyz"]);
    let out = run(&repo, &home, &["merge", "--no-ff", "feat"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(subject(&repo), "Merge branch 'feat' into main", "empty value clears prior patterns");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

// ---------------------------------------------------------------------------
// branch.<name>.mergeOptions (builtin/merge.c:641-659, :667-674, :1407-1408)
// ---------------------------------------------------------------------------

/// `branch.<current>.mergeoptions` is not a typed setting but a *command line*:
/// `parse_branch_merge_options()` splits it with `split_cmdline()` and runs the
/// whole `builtin_merge_options` table over it before `parse_options()` sees the
/// real argv. `--no-ff` from the config therefore turns a fast-forwardable merge
/// into a real merge commit.
#[test]
fn branch_merge_options_supply_merge_defaults() {
    let (repo, home) = fixture("bmo-noff");
    git(&repo, &["config", "branch.main.mergeOptions", "--no-ff"]);
    let out = run(&repo, &home, &["merge", "feat"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(head_parents(&repo), 2, "config --no-ff must defeat the fast-forward");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// Both parses write the same variables, and the command line runs second, so a
/// spelled `--ff` beats the configured `--no-ff`.
#[test]
fn branch_merge_options_lose_to_the_command_line() {
    let (repo, home) = fixture("bmo-override");
    git(&repo, &["config", "branch.main.mergeOptions", "--no-ff"]);
    let out = run(&repo, &home, &["merge", "--ff", "feat"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(head_parents(&repo), 1, "the command line is parsed after the config");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// The key is keyed on the branch `HEAD` currently names, so another branch's
/// entry is never consulted. Guards against reading `branch.*.mergeoptions`
/// wholesale.
#[test]
fn branch_merge_options_are_keyed_on_the_current_branch() {
    let (repo, home) = fixture("bmo-otherbranch");
    git(&repo, &["config", "branch.feat.mergeOptions", "--no-ff"]);
    let out = run(&repo, &home, &["merge", "feat"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(head_parents(&repo), 1, "branch.feat.* must not apply while on main");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `parse_branch_merge_options()` throws away the argv `parse_options()` leaves
/// behind, so a non-option word in the string is *not* a head to merge — it is
/// silently ignored while the options beside it still apply.
#[test]
fn branch_merge_options_drop_non_option_words() {
    let (repo, home) = fixture("bmo-junk");
    git(&repo, &["config", "branch.main.mergeOptions", "--no-ff zzjunk"]);
    let out = run(&repo, &home, &["merge", "feat"]);
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "stderr: {err}");
    assert!(!err.contains("zzjunk"), "the stray word must not become a head:\n{err}");
    assert_eq!(head_parents(&repo), 2, "the --no-ff beside it still applies");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `split_cmdline()` failing is `die(_("Bad branch.%s.mergeoptions string: %s"))`
/// — note the lowercase `mergeoptions` the format string hard-codes whatever the
/// spelling in the file was. Both `split_cmdline` errors are reachable.
#[test]
fn branch_merge_options_report_a_bad_string() {
    for (value, reason) in [("--no-ff \"unclosed", "unclosed quote"), ("--no-ff \\", "cmdline ends with \\")] {
        let (repo, home) = fixture(&format!("bmo-bad{}", reason.len()));
        git(&repo, &["config", "branch.main.mergeOptions", value]);
        let out = run(&repo, &home, &["merge", "feat"]);
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            format!("fatal: Bad branch.main.mergeoptions string: {reason}\n"),
        );
        assert_eq!(out.status.code(), Some(128), "die() is 128");
        let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    }
}

/// An option the merge table does not know is refused by the same parse-options
/// machinery the command line goes through — `error: unknown option` and 129,
/// before any merge is attempted.
#[test]
fn branch_merge_options_refuse_an_unknown_option() {
    let (repo, home) = fixture("bmo-unknown");
    git(&repo, &["config", "branch.main.mergeOptions", "--zzbogus"]);
    let out = run(&repo, &home, &["merge", "feat"]);
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(err.starts_with("error: unknown option `zzbogus'\n"), "err:\n{err}");
    assert_eq!(out.status.code(), Some(129), "parse-options usage failure");
    assert_eq!(subject(&repo), "base", "nothing was merged");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
