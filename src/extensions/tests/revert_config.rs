//! `git revert` honors `revert.reference` as the default for `--reference`,
//! with the command line still overriding (`--no-reference`). The config, like
//! git's `git_revert_config`, only supplies the default: the generated revert
//! commit message uses the `<short> (<subject>, <date>)` reference form when the
//! config is on, and the full `This reverts commit <full-hash>.` form otherwise.
//! Every case is checked byte-for-byte against the system `git`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A fixed, deterministic identity + date so both the reverted commit's short
/// date (SHORT format, `YYYY-MM-DD`) and the abbreviated hash are stable across
/// machines and runs.
const DATE: &str = "1112911993 +0000"; // 2005-04-07 in UTC

fn run(bin: &str, repo: &Path, home: &Path, args: &[&str]) -> Output {
    let mut a: Vec<&str> = Vec::new();
    a.extend_from_slice(args);
    Command::new(bin)
        .args(&a)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("ZVCS_HOME", home)
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("GIT_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", DATE)
        .env("GIT_COMMITTER_DATE", DATE)
        .output()
        .unwrap()
}

/// Build a two-commit repo whose tip (`append line3`) is a clean, trivially
/// reversible change, so the revert never needs a content-level merge.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-revertcfg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();

    // Use the system git only to construct the fixture, under the same fixed
    // env so the reverted commit's hash and date are identical for both binaries.
    run("git", &repo, &home, &["init", "-q", "-b", "main"]);
    run("git", &repo, &home, &["config", "user.name", "A U Thor"]);
    run("git", &repo, &home, &["config", "user.email", "author@example.com"]);
    std::fs::write(repo.join("file.txt"), "line1\nline2\n").unwrap();
    run("git", &repo, &home, &["add", "file.txt"]);
    run("git", &repo, &home, &["commit", "-q", "-m", "add file with two lines"]);
    std::fs::write(repo.join("file.txt"), "line1\nline2\nline3\n").unwrap();
    run("git", &repo, &home, &["add", "file.txt"]);
    run("git", &repo, &home, &["commit", "-q", "-m", "append line3"]);
    (repo, home)
}

/// The reverted commit is reset back to the tip before each measured run so the
/// two binaries revert the identical commit into the identical starting state.
fn reset_to_tip(repo: &Path, home: &Path) {
    run("git", repo, home, &["reset", "--hard", "-q", "HEAD"]);
    // Drop any revert commit a prior run left on the branch.
    run("git", repo, home, &["checkout", "-q", "-B", "main", "HEAD"]);
}

fn head_message(repo: &Path, home: &Path) -> String {
    let o = run("git", repo, home, &["log", "-1", "--format=%B"]);
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// Run `revert <args> HEAD` with `bin`, returning the resulting HEAD commit
/// message. Each call fully rebuilds the fixture so the two binaries start from
/// byte-identical repositories.
fn revert_message(bin: &str, tag: &str, config: Option<bool>, extra: &[&str]) -> String {
    let (repo, home) = fixture(tag);
    if let Some(v) = config {
        run("git", &repo, &home, &["config", "revert.reference", if v { "true" } else { "false" }]);
    }
    reset_to_tip(&repo, &home);
    let mut args = vec!["revert"];
    args.extend_from_slice(extra);
    args.push("HEAD");
    let o = run(bin, &repo, &home, &args);
    assert!(
        o.status.success(),
        "{bin} revert failed: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    let msg = head_message(&repo, &home);
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    msg
}

/// `revert.reference=true` makes the generated message use the reference form,
/// byte-for-byte matching git. This is the config-as-default path.
#[test]
fn revert_reference_config_default_matches_git() {
    let zvcs = revert_message(BIN, "cfg-true-zvcs", Some(true), &[]);
    let real = revert_message("git", "cfg-true-real", Some(true), &[]);
    assert_eq!(real, zvcs, "revert.reference=true default must match git byte-for-byte");
    // The reference form cites the commit as `<short> (<subject>, <date>)`, and
    // omits the full `This reverts commit <40-hex>.` sentence.
    assert!(
        zvcs.contains("(append line3, 2005-04-07)"),
        "reference form cites subject+date:\n{zvcs}"
    );
    assert!(
        zvcs.starts_with("# *** SAY WHY WE ARE REVERTING ON THE TITLE LINE ***"),
        "reference form keeps git's placeholder title:\n{zvcs}"
    );
}

/// `--no-reference` on the command line overrides `revert.reference=true`,
/// restoring the full-hash form; still byte-for-byte against git.
#[test]
fn revert_no_reference_overrides_config() {
    let zvcs = revert_message(BIN, "override-zvcs", Some(true), &["--no-reference"]);
    let real = revert_message("git", "override-real", Some(true), &["--no-reference"]);
    assert_eq!(real, zvcs, "--no-reference must override config and match git");
    assert!(
        zvcs.starts_with("Revert \"append line3\""),
        "override uses the full Revert form:\n{zvcs}"
    );
    assert!(
        zvcs.contains("This reverts commit "),
        "override cites the full-hash sentence:\n{zvcs}"
    );
    assert!(
        !zvcs.contains("SAY WHY WE ARE REVERTING"),
        "override drops the reference placeholder title:\n{zvcs}"
    );
}

/// With neither the config nor a flag, revert stays on git's default full-hash
/// form — a guard that seeding the config default never leaks into the unset
/// case.
#[test]
fn revert_default_unset_stays_full_hash() {
    let zvcs = revert_message(BIN, "unset-zvcs", None, &[]);
    let real = revert_message("git", "unset-real", None, &[]);
    assert_eq!(real, zvcs, "unset default must match git (full-hash form)");
    assert!(
        zvcs.starts_with("Revert \"append line3\""),
        "unset default is the full Revert form:\n{zvcs}"
    );
}

/// `revert.reference=false` is the same as unset (full-hash form); `--reference`
/// still overrides it back to the reference form. Confirms precedence both ways.
#[test]
fn revert_reference_flag_overrides_config_false() {
    let zvcs = revert_message(BIN, "cfgfalse-zvcs", Some(false), &["--reference"]);
    let real = revert_message("git", "cfgfalse-real", Some(false), &["--reference"]);
    assert_eq!(real, zvcs, "--reference must override revert.reference=false and match git");
    assert!(
        zvcs.contains("(append line3, 2005-04-07)"),
        "flag forces the reference form:\n{zvcs}"
    );
}
