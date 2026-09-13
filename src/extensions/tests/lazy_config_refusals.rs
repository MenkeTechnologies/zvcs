//! Config values git refuses only when the code that reads them runs, and the
//! order command-line values are refused in.
//!
//! Each case below diverged from git 2.55.0 before the fix it pins:
//!
//! * `git_config_from_parameters()` (config.c:731-790) hands the callback every
//!   `-c` in the order it was typed. The port walked valued overrides ahead of
//!   valueless ones, so the second refusal of a pair was the one reported.
//! * A trailing blank in `-c key=value ` survives in git and made the value
//!   unreadable; the port read it back through gitoxide's re-parse, which drops
//!   the blank, and accepted `feature.experimental=' '`.
//! * `core.logAllRefUpdates` is read when the ref store is built
//!   (refs.c:2322-2342), `core.warnAmbiguousRefs` when a ref name is dwimmed
//!   (refs.c:828), `core.packedRefsTimeout` when `packed-refs` is locked for a
//!   deletion (refs/packed-backend.c:1222-1228). A bad value kills the commands
//!   that reach the read and no others; the port killed none.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("ZVCS_HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .output()
        .expect("run zvcs git")
}

/// A repository with one commit on `main`.
fn repo(tag: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let root = std::env::temp_dir().join(format!("zvcs-lazycfg-{tag}-{}-{unique}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("mkdir fixture");
    let root = root.canonicalize().expect("canonicalize fixture");
    assert!(run(&root, &["init", "-q", "-b", "main"]).status.success());
    std::fs::write(root.join("a"), "a\n").expect("write a");
    assert!(run(&root, &["add", "a"]).status.success());
    assert!(run(&root, &["commit", "-q", "-m", "a"]).status.success());
    root
}

fn exit_and_first_stderr_line(out: &Output) -> (Option<i32>, String) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    (out.status.code(), stderr.lines().next().unwrap_or_default().to_owned())
}

#[test]
fn command_line_values_are_refused_in_the_order_they_were_typed() {
    let dir = repo("order");
    let out = run(&dir, &["-c", "core.editor", "-c", "core.abbrev=bogus", "branch"]);
    assert_eq!(
        exit_and_first_stderr_line(&out),
        (Some(128), "error: missing value for 'core.editor'".to_owned())
    );
    let out = run(&dir, &["-c", "core.abbrev=bogus", "-c", "core.editor", "branch"]);
    assert_eq!(
        exit_and_first_stderr_line(&out),
        (Some(128), "fatal: bad numeric config value 'bogus' for 'core.abbrev': invalid unit".to_owned())
    );
}

#[test]
fn a_trailing_blank_in_a_command_line_value_is_kept() {
    let dir = repo("blank");
    let out = run(&dir, &["-c", "feature.experimental= ", "rev-parse", "--git-dir"]);
    assert_eq!(
        exit_and_first_stderr_line(&out),
        (Some(128), "fatal: bad boolean config value ' ' for 'feature.experimental'".to_owned())
    );
}

#[test]
fn log_all_ref_updates_is_refused_where_the_ref_store_is_first_used() {
    let dir = repo("logall");
    let quiet = run(&dir, &["-c", "core.logAllRefUpdates=none", "rev-parse", "--git-dir"]);
    assert_eq!(quiet.status.code(), Some(0), "{}", String::from_utf8_lossy(&quiet.stderr));
    let quiet = run(&dir, &["-c", "core.logAllRefUpdates=none", "ls-files"]);
    assert_eq!(quiet.status.code(), Some(0), "{}", String::from_utf8_lossy(&quiet.stderr));
    let out = run(&dir, &["-c", "core.logAllRefUpdates=none", "branch"]);
    assert_eq!(
        exit_and_first_stderr_line(&out),
        (Some(128), "fatal: bad boolean config value 'none' for 'core.logallrefupdates'".to_owned())
    );
    let fine = run(&dir, &["-c", "core.logAllRefUpdates=ALWAYS", "branch"]);
    assert_eq!(fine.status.code(), Some(0), "{}", String::from_utf8_lossy(&fine.stderr));
}

#[test]
fn warn_ambiguous_refs_is_refused_when_a_ref_name_is_dwimmed() {
    let dir = repo("warnamb");
    let out = run(&dir, &["-c", "core.warnAmbiguousRefs==", "rev-parse", "main"]);
    assert_eq!(
        exit_and_first_stderr_line(&out),
        (Some(128), "fatal: bad boolean config value '=' for 'core.warnambiguousrefs'".to_owned())
    );
    let quiet = run(&dir, &["-c", "core.warnAmbiguousRefs==", "branch"]);
    assert_eq!(quiet.status.code(), Some(0), "{}", String::from_utf8_lossy(&quiet.stderr));
}

#[test]
fn packed_refs_timeout_is_refused_only_by_a_deletion() {
    let dir = repo("packedto");
    let create = run(&dir, &["-c", "core.packedRefsTimeout=", "update-ref", "refs/heads/x", "HEAD"]);
    assert_eq!(create.status.code(), Some(0), "{}", String::from_utf8_lossy(&create.stderr));
    let out = run(&dir, &["-c", "core.packedRefsTimeout=", "update-ref", "-d", "refs/heads/x"]);
    assert_eq!(
        exit_and_first_stderr_line(&out),
        (Some(128), "fatal: bad numeric config value '' for 'core.packedrefstimeout': invalid unit".to_owned())
    );
    assert!(dir.join(".git/refs/heads/x").is_file(), "the refused deletion must leave the ref");
}
