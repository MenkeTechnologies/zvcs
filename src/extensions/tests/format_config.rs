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
        Command::new(BIN).args(args).current_dir(dir).status().unwrap().success(),
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

/// `format.signOff` defaults `-s`, and `--no-signoff` turns it back off. The
/// trailer names the committer identity and is separated from the subject by a
/// blank line (`append_signoff()` with an empty body).
#[test]
fn format_signoff_config_and_override() {
    let (repo, home) = fixture("signoff");
    git(&repo, &["config", "format.signOff", "true"]);

    let out = fmt(&repo, &home, &[]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.contains("first change\n\nSigned-off-by: t <t@e.x>\n---\n"),
        "config signoff trailer:\n{s}"
    );

    let out = fmt(&repo, &home, &["--no-signoff"]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(!s.contains("Signed-off-by"), "--no-signoff wins:\n{s}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `format.from` replaces the `From:` header and pushes the commit's author into
/// an in-body `From:`; a boolean `true` means the committer identity. When the
/// two identities already agree the in-body header is dropped unless
/// `format.forceInBodyFrom` asks for it.
#[test]
fn format_from_and_force_in_body_from_config() {
    let (repo, home) = fixture("from");
    git(&repo, &["config", "format.from", "Relay <relay@x.y>"]);

    let out = fmt(&repo, &home, &[]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert_eq!(line_with(&s, "From: R"), Some("From: Relay <relay@x.y>"));
    assert!(
        s.contains("first change\n\nFrom: t <t@e.x>\n"),
        "author moved in-body:\n{s}"
    );

    // A boolean `true` is the committer identity, which here *is* the author, so
    // no in-body header is emitted.
    git(&repo, &["config", "format.from", "true"]);
    let out = fmt(&repo, &home, &[]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert_eq!(line_with(&s, "From:"), Some("From: t <t@e.x>"));
    assert_eq!(s.matches("From: t <t@e.x>").count(), 1, "no in-body From:\n{s}");

    git(&repo, &["config", "format.forceInBodyFrom", "true"]);
    let out = fmt(&repo, &home, &[]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        s.matches("From: t <t@e.x>").count(),
        2,
        "forceInBodyFrom repeats the identity in-body:\n{s}"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `format.thread` mints a `Message-ID:`; `--no-thread` suppresses it. The id
/// embeds `time(NULL)`, so only its shape is pinned.
#[test]
fn format_thread_config_and_override() {
    let (repo, home) = fixture("thread");
    git(&repo, &["config", "format.thread", "deep"]);

    let out = fmt(&repo, &home, &[]);
    let s = String::from_utf8_lossy(&out.stdout);
    let id = line_with(&s, "Message-ID:").expect("threading mints a Message-ID");
    assert!(id.ends_with(".git.t@e.x>"), "id carries the committer mail: {id}");

    let out = fmt(&repo, &home, &["--no-thread"]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(!s.contains("Message-ID:"), "--no-thread wins:\n{s}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `format.attach` names the MIME boundary and selects `attachment` disposition;
/// `--inline` keeps the boundary but switches the disposition, and `--no-attach`
/// drops the multipart wrapper entirely.
#[test]
fn format_attach_config_and_override() {
    let (repo, home) = fixture("attach");
    git(&repo, &["config", "format.attach", "BND"]);

    let out = fmt(&repo, &home, &[]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.contains("Content-Type: multipart/mixed; boundary=\"------------BND\"\n"),
        "config boundary:\n{s}"
    );
    assert!(s.contains("Content-Disposition: attachment;"), "attachment:\n{s}");
    assert!(s.ends_with("\n--------------BND--\n\n\n"), "closing boundary:\n{s}");

    let out = fmt(&repo, &home, &["--inline"]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("Content-Disposition: inline;"), "--inline:\n{s}");

    let out = fmt(&repo, &home, &["--no-attach"]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(!s.contains("multipart/mixed"), "--no-attach wins:\n{s}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `format.notes` selects the notes tree whose `Notes:` block is appended to the
/// message; a ref value narrows it to that ref and labels the block with it.
#[test]
fn format_notes_config_and_override() {
    let (repo, home) = fixture("notes");
    git(&repo, &["notes", "add", "-m", "default note", "HEAD"]);
    git(&repo, &["notes", "--ref=side", "add", "-m", "side note", "HEAD"]);

    git(&repo, &["config", "format.notes", "true"]);
    let out = fmt(&repo, &home, &[]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("---\n\nNotes:\n    default note\n"), "default tree:\n{s}");

    git(&repo, &["config", "format.notes", "side"]);
    let out = fmt(&repo, &home, &[]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("---\n\nNotes (side):\n    side note\n"), "named tree:\n{s}");
    assert!(!s.contains("default note"), "explicit ref suppresses default:\n{s}");

    let out = fmt(&repo, &home, &["--no-notes"]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(!s.contains("Notes"), "--no-notes wins:\n{s}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `format.coverFromDescription` decides how `branch.<name>.description` feeds
/// the cover letter, and `format.commitListFormat` replaces the shortlog.
#[test]
fn format_cover_from_description_and_commit_list_format() {
    let (repo, home) = fixture("cover");
    git(&repo, &["config", "branch.main.description", "Desc subject\n\nDesc body.\n"]);

    // The default (`message`) keeps the placeholder subject and uses the whole
    // description as the blurb.
    let out = fmt(&repo, &home, &["--cover-letter"]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("[PATCH 0/1] *** SUBJECT HERE ***"), "placeholder kept:\n{s}");
    assert!(s.contains("\nDesc subject\n\nDesc body.\n"), "description as blurb:\n{s}");

    git(&repo, &["config", "format.coverFromDescription", "subject"]);
    let out = fmt(&repo, &home, &["--cover-letter"]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("[PATCH 0/1] Desc subject"), "subject taken over:\n{s}");
    assert!(!s.contains("*** BLURB HERE ***"), "blurb replaced:\n{s}");

    git(&repo, &["config", "format.coverFromDescription", "none"]);
    let out = fmt(&repo, &home, &["--cover-letter"]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("*** BLURB HERE ***"), "description ignored:\n{s}");

    git(&repo, &["config", "format.commitListFormat", "modern"]);
    let out = fmt(&repo, &home, &["--cover-letter"]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("\n[1/1] first change\n"), "modern commit list:\n{s}");
    assert!(!s.contains("t (1):"), "shortlog replaced:\n{s}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `format.useAutoBase=whenAble` degrades silently when no upstream can supply a
/// base, while `true` makes the same situation fatal — the two halves of git's
/// `die_on_failure` in `get_base_commit()`.
#[test]
fn format_use_auto_base_config() {
    let (repo, home) = fixture("autobase");

    git(&repo, &["config", "format.useAutoBase", "whenAble"]);
    let out = fmt(&repo, &home, &[]);
    assert_eq!(out.status.code(), Some(0), "whenAble tolerates no upstream");
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(!s.contains("base-commit:"), "no base recorded:\n{s}");

    git(&repo, &["config", "format.useAutoBase", "true"]);
    let out = fmt(&repo, &home, &[]);
    assert_eq!(out.status.code(), Some(128), "true is fatal without an upstream");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.starts_with("fatal: failed to get upstream"), "git's message:\n{err}");

    // An explicit --base wins over the config and records the trailer.
    git(&repo, &["config", "format.useAutoBase", "false"]);
    std::fs::write(repo.join("f"), "b\n").unwrap();
    git(&repo, &["commit", "-q", "-am", "second change"]);
    let out = fmt(&repo, &home, &["--base=HEAD~1"]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("\nbase-commit: "), "explicit base recorded:\n{s}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
