//! Two message-rendering rules that only show up on real commit text: `%xNN`
//! byte escapes, and the tab expansion the indenting pretty formats apply.
//!
//! Both were found by diffing whole-history output against stock git, and both
//! are invisible on the small synthetic messages most tests use — a message has
//! to actually contain a tab, and a format has to actually ask for a byte the
//! shell would otherwise eat.
//!
//! Every test builds its own repository under a private `ZVCS_HOME`, so nothing
//! here reads the developer's ledger or the surrounding repository.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn zvcs(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .expect("run zvcs git")
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A repository whose single commit carries `message`.
fn repo_with_message(tag: &str, message: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-logfmt-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    let home = root.join("home");
    std::fs::create_dir_all(&repo).expect("mkdir repo");
    std::fs::create_dir_all(&home).expect("mkdir home");

    assert!(zvcs(&repo, &home, &["init", "-q", "-b", "main"]).status.success(), "init");
    std::fs::write(repo.join("a.txt"), b"hello\n").expect("write file");
    assert!(zvcs(&repo, &home, &["add", "a.txt"]).status.success(), "add");

    let msg_file = root.join("msg");
    std::fs::write(&msg_file, message).expect("write message");
    let commit = zvcs(&repo, &home, &["commit", "-q", "-F", msg_file.to_str().unwrap()]);
    assert!(commit.status.success(), "commit: {}", String::from_utf8_lossy(&commit.stderr));
    (repo, home)
}

/// `%xNN` emits the byte those two hex digits name, in either case. This is the
/// only way a format can ask for a tab or a space that survives the shell.
#[test]
fn hex_escapes_emit_their_byte() {
    let (repo, home) = repo_with_message("hex", "subject line\n");

    let out = zvcs(&repo, &home, &["log", "--format=%x41%x09%x62%x0A%x4a"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    // A, TAB, b, LF, J — the record's own newline follows.
    assert_eq!(stdout_of(&out), "A\tb\nJ\n");
}

/// A `%x` that is not followed by two hex digits is not a placeholder at all:
/// git prints the text as typed rather than failing the command.
#[test]
fn malformed_hex_escapes_print_literally() {
    let (repo, home) = repo_with_message("hexbad", "subject line\n");

    let out = zvcs(&repo, &home, &["log", "--format=%xZZ|%x4|%x"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(stdout_of(&out), "%xZZ|%x4|%x\n");
}

/// The indenting formats expand tabs to 8-column stops, measuring from the
/// message's own left edge — the four-space indent does not shift the stops.
/// Without this, a message whose lines were aligned with tabs comes out ragged.
#[test]
fn indented_formats_expand_tabs_from_the_message_edge() {
    // "ab" + TAB lands at column 8; "abcdefgh" + TAB lands at column 16.
    let (repo, home) = repo_with_message("tabs", "subject\n\nab\tX\nabcdefgh\tY\n");

    let out = zvcs(&repo, &home, &["log", "--format=medium"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = stdout_of(&out);
    assert!(text.contains("    ab      X\n"), "tab after 2 columns pads to 8: {text:?}");
    assert!(text.contains("    abcdefgh        Y\n"), "tab at column 8 pads to 16: {text:?}");
    assert!(!text.contains('\t'), "no tab survives an indenting format: {text:?}");
}

/// `raw` has no tab width in git's format table, so it prints the message's
/// bytes unchanged — the reason the expansion is a per-format setting and not a
/// property of indenting.
#[test]
fn raw_format_leaves_tabs_alone() {
    let (repo, home) = repo_with_message("tabsraw", "subject\n\nab\tX\n");

    let out = zvcs(&repo, &home, &["log", "--pretty=raw"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout_of(&out).contains("    ab\tX\n"), "raw keeps the tab: {:?}", stdout_of(&out));
}

/// `--oneline` has no tab width either, and its subject is not indented.
#[test]
fn oneline_leaves_tabs_alone() {
    let (repo, home) = repo_with_message("tabsone", "sub\tject\n");

    let out = zvcs(&repo, &home, &["log", "--oneline"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout_of(&out).contains("sub\tject"), "oneline keeps the tab: {:?}", stdout_of(&out));
}

/// The parallel record renderer must not change what is rendered: pinning the
/// worker count to 1 and letting it fan out have to produce identical bytes.
/// The commits differ in message length so the workers do unequal work.
#[test]
fn worker_count_does_not_change_output() {
    let root = std::env::temp_dir().join(format!("zvcs-logfmt-threads-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    let home = root.join("home");
    std::fs::create_dir_all(&repo).expect("mkdir repo");
    std::fs::create_dir_all(&home).expect("mkdir home");
    assert!(zvcs(&repo, &home, &["init", "-q", "-b", "main"]).status.success(), "init");

    for i in 0..60 {
        std::fs::write(repo.join("a.txt"), format!("line {i}\n")).expect("write file");
        assert!(zvcs(&repo, &home, &["add", "a.txt"]).status.success(), "add");
        let msg = format!("commit {i}\n\n{}\n", "body ".repeat(i % 7 + 1));
        let msg_file = root.join("msg");
        std::fs::write(&msg_file, msg).expect("write message");
        assert!(
            zvcs(&repo, &home, &["commit", "-q", "-F", msg_file.to_str().unwrap()])
                .status
                .success(),
            "commit {i}"
        );
    }

    let parallel = zvcs(&repo, &home, &["log", "--format=%h %s|%b"]);
    let sequential = Command::new(BIN)
        .args(["log", "--format=%h %s|%b"])
        .current_dir(&repo)
        .env("ZVCS_HOME", &home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("ZVCS_THREADS", "1")
        .output()
        .expect("run zvcs git");

    assert!(parallel.status.success() && sequential.status.success());
    assert_eq!(
        stdout_of(&parallel),
        stdout_of(&sequential),
        "fanning the record rendering out must not reorder or alter it"
    );
}
