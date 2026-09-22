//! How `.git/shallow` is read — `is_repository_shallow()` (`shallow.c:63-95`).
//!
//! ```c
//! while (fgets(buf, sizeof(buf), fp)) {
//!         struct object_id oid;
//!         if (get_oid_hex(buf, &oid))
//!                 die("bad shallow line: %s", buf);
//!         register_shallow(r, &oid);
//! }
//! ```
//!
//! `fgets` leaves the newline in `buf` and `get_oid_hex()` consumes exactly
//! `the_hash_algo->hexsz` characters without looking at what follows, so a line
//! is a boundary as long as it *starts* with a full object id. That leniency is
//! not decoration: it is what lets the same file work when it picks up a
//! trailing space or a CR, and a reader that insists on the whole line instead
//! quietly decides the repository is no longer shallow — which does not fail
//! until the first parent the repository does not have.
//!
//! Every expectation below was captured from stock git 2.55.0 against a
//! `--depth=2` clone of a five-commit line.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-shallow-boundary-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn git_in(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .current_dir(dir)
        .output()
        .expect("run the binary under test")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A five-commit line, and a repository grafted two commits in — built by hand
/// rather than by cloning so the test needs no transport.
///
/// Returns the working repository and the object id its `.git/shallow` names.
fn grafted(name: &str) -> (PathBuf, String) {
    let root = scratch(name);
    let repo = root.join("work");
    std::fs::create_dir_all(&repo).unwrap();
    let out = git_in(&repo, &["init", "-q", "-b", "main", "."]);
    assert!(out.status.success(), "init: {}", stderr(&out));
    for i in 1..=5 {
        std::fs::write(repo.join("f"), format!("{i}\n")).unwrap();
        git_in(&repo, &["add", "f"]);
        let out = git_in(&repo, &["commit", "-q", "-m", &format!("c{i}")]);
        assert!(out.status.success(), "commit c{i}: {}", stderr(&out));
    }
    let boundary = stdout(&git_in(&repo, &["rev-parse", "HEAD~1"])).trim().to_owned();
    assert_eq!(boundary.len(), 40, "a full object id: {boundary}");
    (repo, boundary)
}

/// The commit subjects `log` reaches, which is the readable form of "where the
/// graft is".
fn log_of(repo: &Path) -> Vec<String> {
    let out = git_in(repo, &["log", "--format=%s"]);
    assert!(out.status.success(), "log: {}", stderr(&out));
    stdout(&out).lines().map(str::to_owned).collect()
}

fn set_shallow(repo: &Path, body: &str) {
    std::fs::write(repo.join(".git").join("shallow"), body).unwrap();
}

/// Measured against git 2.55.0, writing the same boundary four ways:
///
/// ```text
/// $ printf '%s\n'   "$b" > .git/shallow && git log --oneline
/// 4a3fb3cba9 c5
/// 46d54d6a68 c4
/// $ printf '%s \n'  "$b" > .git/shallow && git log --oneline
/// 4a3fb3cba9 c5
/// 46d54d6a68 c4
/// $ printf '%s\r\n' "$b" > .git/shallow && git log --oneline
/// 4a3fb3cba9 c5
/// 46d54d6a68 c4
/// ```
///
/// All three graft at the same place. A reader that decodes the whole line
/// instead drops the graft for the last two and then dies walking into the
/// history it does not have:
///
/// ```text
/// error: Could not read 1fda4da119b297fdf93a4d4f9cd9647321f5c125
/// fatal: Failed to traverse parents of commit 46d54d6a684e1a8144b2cbf2d98c6ee0ad47e8d6
/// ```
#[test]
fn a_boundary_line_may_carry_trailing_bytes() {
    let (repo, boundary) = grafted("trailing");
    let full = log_of(&repo);
    assert_eq!(full.len(), 5, "ungrafted, the whole line is visible: {full:?}");

    for (label, body) in [
        ("plain", format!("{boundary}\n")),
        ("trailing space", format!("{boundary} \n")),
        ("carriage return", format!("{boundary}\r\n")),
        ("no newline at all", boundary.clone()),
        ("a word after the id", format!("{boundary} why-not\n")),
    ] {
        set_shallow(&repo, &body);
        assert_eq!(
            log_of(&repo),
            vec!["c5".to_string(), "c4".to_string()],
            "{label}: the graft holds and the walk stops at the boundary"
        );
    }
}

/// The graft is what stops the walk, so removing the file restores the full
/// history — the control that proves the assertions above are measuring the
/// boundary and not something that happens to truncate the log.
#[test]
fn removing_the_boundary_file_restores_the_history() {
    let (repo, boundary) = grafted("removal");
    set_shallow(&repo, &format!("{boundary} \n"));
    assert_eq!(log_of(&repo).len(), 2);

    std::fs::remove_file(repo.join(".git").join("shallow")).unwrap();
    assert_eq!(log_of(&repo).len(), 5, "without the file nothing is grafted");
}

/// `rev-parse --is-shallow-repository` answers for the *file*, not for its
/// contents: `is_repository_shallow()` sets `is_shallow = 1` as soon as the
/// `fopen` succeeds (`shallow.c:79-85`), before a single line is parsed.
///
/// Measured against git 2.55.0 with an empty `.git/shallow`:
///
/// ```text
/// $ : > .git/shallow && git rev-parse --is-shallow-repository
/// true
/// ```
#[test]
fn an_empty_boundary_file_still_makes_the_repository_shallow() {
    let (repo, _) = grafted("empty");
    set_shallow(&repo, "");
    assert_eq!(
        stdout(&git_in(&repo, &["rev-parse", "--is-shallow-repository"])).trim(),
        "true"
    );
    assert_eq!(
        log_of(&repo).len(),
        5,
        "with no line in it there is no graft, so nothing is cut"
    );

    std::fs::remove_file(repo.join(".git").join("shallow")).unwrap();
    assert_eq!(
        stdout(&git_in(&repo, &["rev-parse", "--is-shallow-repository"])).trim(),
        "false"
    );
}
