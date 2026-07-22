//! `git format-patch` honors the `format.*` config as option defaults —
//! `format.subjectPrefix`, `format.to`, `format.cc`, `format.signature`,
//! `format.signatureFile` — with the CLI overriding scalars and appending to the
//! address lists. Regression guard for these being hardcoded (`[PATCH]`, empty
//! To/Cc, the version-string signature).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-fmtcfg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@e.x"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("f"), "a\n").unwrap();
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-q", "-m", "first change"]);
    (repo, home)
}

fn fmt(repo: &Path, home: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["format-patch", "--stdout", "-1"];
    args.extend_from_slice(extra);
    Command::new(BIN)
        .args(&args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .output()
        .unwrap()
}

fn line_with<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    text.lines().find(|l| l.starts_with(prefix))
}

#[test]
fn format_subject_prefix_config_and_override() {
    let (repo, home) = fixture("subject");
    git(&repo, &["config", "format.subjectPrefix", "RFC"]);
    let out = fmt(&repo, &home, &[]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert_eq!(line_with(&s, "Subject:"), Some("Subject: [RFC] first change"));

    // --subject-prefix overrides the config.
    let out = fmt(&repo, &home, &["--subject-prefix", "CUSTOM"]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert_eq!(line_with(&s, "Subject:"), Some("Subject: [CUSTOM] first change"));

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn format_to_cc_config_and_append() {
    let (repo, home) = fixture("tocc");
    git(&repo, &["config", "format.to", "Alice <a@x.y>"]);
    git(&repo, &["config", "format.cc", "Bob <b@x.y>"]);

    let out = fmt(&repo, &home, &[]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert_eq!(line_with(&s, "To:"), Some("To: Alice <a@x.y>"));
    assert_eq!(line_with(&s, "Cc:"), Some("Cc: Bob <b@x.y>"));

    // --to appends to the config value (folded header keeps Alice first).
    let out = fmt(&repo, &home, &["--to", "Carol <c@x.y>"]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("Alice <a@x.y>"), "config To retained:\n{s}");
    assert!(s.contains("Carol <c@x.y>"), "--to appended:\n{s}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `format.signature` replaces the default version-string trailer, and an
/// explicit `--signature`/`--no-signature` overrides the config (git's
/// `signature`-pointer tier beats `cfg.signature`).
#[test]
fn format_signature_config_and_override() {
    let (repo, home) = fixture("sig");
    git(&repo, &["config", "format.signature", "CFGSIG"]);

    let out = fmt(&repo, &home, &[]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("-- \nCFGSIG\n"), "config signature trailer:\n{s}");

    // --signature overrides the config value.
    let out = fmt(&repo, &home, &["--signature", "CLISIG"]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("-- \nCLISIG\n"), "--signature overrides config:\n{s}");
    assert!(!s.contains("CFGSIG"), "config value dropped:\n{s}");

    // --no-signature suppresses the trailer entirely.
    let out = fmt(&repo, &home, &["--no-signature"]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(!s.contains("CFGSIG"), "--no-signature suppresses config:\n{s}");
    assert!(!s.contains("\n-- \n"), "no signature separator emitted:\n{s}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `format.signatureFile` reads the trailer from disk, but only when
/// `format.signature` is unset — the two config keys and the `--signature`/
/// `--signature-file` CLI options resolve in git's documented ladder.
#[test]
fn format_signature_file_config_and_precedence() {
    let (repo, home) = fixture("sigfile");
    std::fs::write(repo.join("sig.txt"), "SIGFROMFILE\nline2\n").unwrap();

    // format.signatureFile alone -> trailer is the file's contents verbatim.
    git(&repo, &["config", "format.signatureFile", "sig.txt"]);
    let out = fmt(&repo, &home, &[]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.contains("-- \nSIGFROMFILE\nline2\n"),
        "signatureFile trailer:\n{s}"
    );

    // format.signature set alongside it wins (the file is not read).
    git(&repo, &["config", "format.signature", "CFGSIG"]);
    let out = fmt(&repo, &home, &[]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("-- \nCFGSIG\n"), "format.signature beats file:\n{s}");
    assert!(!s.contains("SIGFROMFILE"), "file not read:\n{s}");

    // A CLI --signature-file is read even when format.signature is set.
    let out = fmt(&repo, &home, &["--signature-file", "sig.txt"]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.contains("-- \nSIGFROMFILE\nline2\n"),
        "--signature-file beats format.signature:\n{s}"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// An unreadable `format.signatureFile` is git's `die_errno` (exit 128) once the
/// series is non-empty; a bad revision, resolved first, preempts it.
#[test]
fn format_signature_file_invalid_errors() {
    let (repo, home) = fixture("sigbad");
    git(&repo, &["config", "format.signatureFile", "nope.txt"]);

    let out = fmt(&repo, &home, &[]);
    assert_eq!(out.status.code(), Some(128), "missing file is fatal");
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        err.trim_end(),
        "fatal: unable to read signature file 'nope.txt': No such file or directory"
    );

    // A bad revision is resolved before the signature file, so it wins.
    let out = Command::new(BIN)
        .args(["format-patch", "--stdout", "NOSUCHREV"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", &home)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(128), "bad revision is fatal");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.starts_with("fatal: ambiguous argument 'NOSUCHREV'"),
        "revision error preempts signature file:\n{err}"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
