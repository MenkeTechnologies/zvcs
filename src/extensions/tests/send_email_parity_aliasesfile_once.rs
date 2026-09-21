//! `sendemail.aliasesfile` is the one list-valued `%config_path_settings` entry,
//! so `@alias_files` holds one element per configured value and
//! `parse_sendmail_aliases()` reads each of them once. A single
//! `-c sendemail.aliasesfile=<file>` is one value, so its warnings appear once.
//!
//! This port's config snapshot delivers each *valued* `-c` on two sources, which
//! [`zvcs::config::CliEcho`] discounts for the ordinary config walks. Without the
//! same discount in `send-email`'s own `%config` scan the file lands in
//! `@alias_files` twice and every `warning: sendmail line is not recognized:` line
//! is printed twice — a doubled diagnostic for a file the user named once.
//!
//! Both halves are pinned: one `-c` warns once, two `-c` of the same file warn
//! twice (stock reads the file once per configured value, so the doubling is
//! correct *there* and must not be discounted away).
//!
//! `--dump-aliases` reads alias files and prints names; it never opens a socket
//! and never sends mail.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// An alias file with one good entry and one line the sendmail parser rejects,
/// so the run produces exactly one warning per read.
const ALIASES: &str = "grp: a@example.com\nbadline without colon\n";

fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-se-alias-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let aliases = root.join("aliases.sendmail");
    std::fs::write(&aliases, ALIASES).unwrap();

    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let ok = Command::new(BIN)
        .args(["init", "-q", "-b", "main"])
        .current_dir(&repo)
        .env("GIT_CEILING_DIRECTORIES", &repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .status()
        .unwrap();
    assert!(ok.success(), "init failed");
    (repo, aliases)
}

fn dump(repo: &Path, config: &[String]) -> Output {
    let mut cmd = Command::new(BIN);
    for c in config {
        cmd.arg("-c").arg(c);
    }
    cmd.args(["send-email", "--dump-aliases"])
        .current_dir(repo)
        .env("HOME", repo)
        .env("ZVCS_HOME", repo)
        .env("GIT_CEILING_DIRECTORIES", repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LC_ALL", "C")
        .output()
        .unwrap()
}

/// One `-c sendemail.aliasesfile=<f>` is one value in `@alias_files`, so the file
/// is read once and its unparsable line is reported once.
#[test]
fn one_c_override_reads_the_alias_file_once() {
    let (repo, aliases) = fixture("once");
    let out = dump(
        &repo,
        &[
            "sendemail.aliasfiletype=sendmail".into(),
            format!("sendemail.aliasesfile={}", aliases.display()),
        ],
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "grp\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "warning: sendmail line is not recognized: badline without colon\n",
    );
    assert_eq!(out.status.code(), Some(0));
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// Two `-c` of the same key are two values, and `parse_sendmail_aliases()` runs
/// once per element of `@alias_files` — so the doubled warning here is stock's
/// behaviour and must survive the echo discount.
#[test]
fn two_c_overrides_read_the_alias_file_twice() {
    let (repo, aliases) = fixture("twice");
    let file = format!("sendemail.aliasesfile={}", aliases.display());
    let out = dump(
        &repo,
        &["sendemail.aliasfiletype=sendmail".into(), file.clone(), file],
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "grp\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "warning: sendmail line is not recognized: badline without colon\n\
         warning: sendmail line is not recognized: badline without colon\n",
    );
    assert_eq!(out.status.code(), Some(0));
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// The alias file named in the repository config (no `-c` involved) was never
/// affected by the echo and must still be read exactly once.
#[test]
fn a_config_file_entry_reads_the_alias_file_once() {
    let (repo, aliases) = fixture("cfgfile");
    for (k, v) in [
        ("sendemail.aliasfiletype", "sendmail".to_owned()),
        ("sendemail.aliasesfile", aliases.display().to_string()),
    ] {
        let ok = Command::new(BIN)
            .args(["config", k, &v])
            .current_dir(&repo)
            .env("GIT_CEILING_DIRECTORIES", &repo)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .status()
            .unwrap();
        assert!(ok.success(), "config {k} failed");
    }
    let out = dump(&repo, &[]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "grp\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "warning: sendmail line is not recognized: badline without colon\n",
    );
    assert_eq!(out.status.code(), Some(0));
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
