//! Parity coverage for `git clean -i`, the interactive prompt loop ported from
//! `builtin/clean.c`. These drive the loop over piped stdin (no TTY needed) and
//! assert on the menu wording, the per-command semantics, and the resulting
//! worktree state. `COLUMNS` is pinned so the column layout is deterministic.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

/// A fresh repo under a unique temp dir, plus the `ZVCS_HOME` to isolate state.
fn fixture(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-clean-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    (root, repo)
}

/// Run `git clean <args>` (the zvcs binary) in `repo`, feeding `input` on stdin.
fn clean_i(repo: &Path, home: &Path, args: &[&str], input: &str) -> (String, String, bool) {
    let mut child = Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .env("ZVCS_HOME", home)
        .env("COLUMNS", "80")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

/// Choosing `clean` (by hotkey `c`) removes every listed candidate and prints the
/// header, the command menu prompt, and a `Removing` line per file.
#[test]
fn interactive_clean_removes_all() {
    let (root, repo) = fixture("clean-all");
    let home = root.join("home");
    std::fs::write(repo.join("foo.txt"), b"x").unwrap();

    let (stdout, stderr, ok) = clean_i(&repo, &home, &["clean", "-i"], "c\n");
    assert!(ok, "clean -i should succeed: {stderr}");
    assert!(
        stdout.contains("Would remove the following item:"),
        "singular header missing:\n{stdout}"
    );
    assert!(stdout.contains("What now> "), "command prompt missing:\n{stdout}");
    assert!(stdout.contains("Removing foo.txt"), "removal line missing:\n{stdout}");
    assert!(!repo.join("foo.txt").exists(), "foo.txt should be gone");

    let _ = std::fs::remove_dir_all(&root);
}

/// Choosing `quit` (by hotkey `q`) prints `Bye.` and removes nothing.
#[test]
fn interactive_quit_removes_nothing() {
    let (root, repo) = fixture("quit");
    let home = root.join("home");
    std::fs::write(repo.join("keep.txt"), b"x").unwrap();

    let (stdout, stderr, ok) = clean_i(&repo, &home, &["clean", "-i"], "q\n");
    assert!(ok, "clean -i quit should succeed: {stderr}");
    assert!(stdout.contains("Bye."), "quit should print Bye.:\n{stdout}");
    assert!(!stdout.contains("Removing"), "quit must not remove anything:\n{stdout}");
    assert!(repo.join("keep.txt").exists(), "keep.txt must survive quit");

    let _ = std::fs::remove_dir_all(&root);
}

/// `ask each` confirms per file: only a prefix of "yes" deletes; anything else
/// (here `n`) spares the file.
#[test]
fn interactive_ask_each_is_selective() {
    let (root, repo) = fixture("ask");
    let home = root.join("home");
    std::fs::write(repo.join("a.txt"), b"x").unwrap();
    std::fs::write(repo.join("b.txt"), b"x").unwrap();

    // "a" -> ask each; then y for a.txt (sorted first), n for b.txt.
    let (stdout, stderr, ok) = clean_i(&repo, &home, &["clean", "-i"], "a\ny\nn\n");
    assert!(ok, "clean -i ask each should succeed: {stderr}");
    assert!(
        stdout.contains("Remove a.txt [y/N]? "),
        "per-file prompt missing:\n{stdout}"
    );
    assert!(stdout.contains("Remove b.txt [y/N]? "), "second prompt missing:\n{stdout}");
    assert!(!repo.join("a.txt").exists(), "a.txt (yes) should be removed");
    assert!(repo.join("b.txt").exists(), "b.txt (no) should survive");

    let _ = std::fs::remove_dir_all(&root);
}
