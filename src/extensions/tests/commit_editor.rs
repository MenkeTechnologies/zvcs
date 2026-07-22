//! `git commit` without `-m` captures the message from an editor, honoring the
//! `GIT_EDITOR`/`core.editor` chain, `commit.template`, `core.commentChar`, and
//! `commit.cleanup`. Regression guard for editor mode being unsupported (it
//! bailed "editor mode is unsupported; use -m"). `GIT_EDITOR` is a script here,
//! so the tests are headless — no interactive editor is ever launched.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

/// A hermetic repo with one staged file, ready to commit.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-ced-{tag}-{}", std::process::id()));
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
    std::fs::write(repo.join("f.txt"), "hello\n").unwrap();
    git(&repo, &["add", "f.txt"]);
    (repo, home)
}

/// Run zvcs `commit` with `GIT_EDITOR` set to a shell snippet that receives the
/// message path as `$1`.
fn commit_with_editor(repo: &Path, home: &Path, editor: &str, extra: &[&str]) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.arg("commit")
        .args(extra)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .env("GIT_EDITOR", editor)
        .stdin(std::process::Stdio::null());
    cmd.output().unwrap()
}

fn subject(repo: &Path) -> String {
    let out = Command::new("git")
        .args(["log", "-1", "--format=%s"])
        .current_dir(repo)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn editor_message_is_used_and_comments_stripped() {
    let (repo, home) = fixture("basic");
    // Editor writes a subject plus a comment line; the comment must be stripped.
    let out = commit_with_editor(
        &repo,
        &home,
        r#"sh -c 'printf "editor subject\n\n# a comment\n" > "$1"' _"#,
        &[],
    );
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(subject(&repo), "editor subject");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn commit_template_seeds_the_message() {
    let (repo, home) = fixture("template");
    let tmpl = repo.join("tmpl");
    std::fs::write(&tmpl, "seeded subject\n").unwrap();
    git(&repo, &["config", "commit.template", tmpl.to_str().unwrap()]);
    // Editor appends nothing — the template alone becomes the message.
    let out = commit_with_editor(&repo, &home, ":", &[]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(subject(&repo), "seeded subject");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn core_comment_char_controls_stripping() {
    let (repo, home) = fixture("commentchar");
    git(&repo, &["config", "core.commentChar", ";"]);
    // With ';' as the comment char, a leading '#' line is real content and kept,
    // while the ';' line is stripped.
    let out = commit_with_editor(
        &repo,
        &home,
        r##"sh -c 'printf "# kept subject\n\n; dropped\n" > "$1"' _"##,
        &[],
    );
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(subject(&repo), "# kept subject");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn empty_message_aborts() {
    let (repo, home) = fixture("empty");
    let out = commit_with_editor(
        &repo,
        &home,
        r##"sh -c 'printf "# only a comment\n" > "$1"' _"##,
        &[],
    );
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Aborting commit due to empty commit message"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn commit_status_false_gives_empty_template() {
    let (repo, home) = fixture("statusfalse");
    git(&repo, &["config", "commit.status", "false"]);
    // The editor records the size of the template it was handed, then writes the
    // real message. With commit.status=false git omits the whole header, so the
    // template is empty (0 bytes).
    let out = commit_with_editor(
        &repo,
        &home,
        r#"sh -c 'wc -c < "$1" | tr -d " " > tsize; printf "the subject\n" > "$1"' _"#,
        &[],
    );
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let size = std::fs::read_to_string(repo.join("tsize")).unwrap();
    assert_eq!(size.trim(), "0", "template must be empty when commit.status=false");
    assert_eq!(subject(&repo), "the subject");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn core_comment_string_multibyte_prefix() {
    let (repo, home) = fixture("commentstring");
    git(&repo, &["config", "core.commentString", "//"]);
    // A `//`-prefixed line is stripped (multi-byte comment prefix), the real
    // subject is kept.
    let out = commit_with_editor(
        &repo,
        &home,
        r#"sh -c 'printf "kept subject\n\n// dropped line\n" > "$1"' _"#,
        &[],
    );
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(subject(&repo), "kept subject");
    let body = Command::new("git")
        .args(["log", "-1", "--format=%b"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&body.stdout).contains("//"),
        "the // comment line must be stripped"
    );
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn no_editor_on_dumb_terminal_refuses() {
    let (repo, home) = fixture("dumb");
    // No editor configured and a non-interactive stdin: git refuses rather than
    // launching a broken editor. (No GIT_EDITOR here.)
    let out = Command::new(BIN)
        .arg("commit")
        .current_dir(&repo)
        .env("HOME", &home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", &home)
        .env("TERM", "dumb")
        .env_remove("GIT_EDITOR")
        .env_remove("EDITOR")
        .env_remove("VISUAL")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Terminal is dumb, but EDITOR unset"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
