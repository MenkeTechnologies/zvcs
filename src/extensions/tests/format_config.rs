//! `git format-patch` honors the `format.*` config as option defaults —
//! `format.subjectPrefix`, `format.to`, `format.cc` — with the CLI overriding
//! scalars and appending to the address lists. Regression guard for these being
//! hardcoded (`[PATCH]`, empty To/Cc).

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
