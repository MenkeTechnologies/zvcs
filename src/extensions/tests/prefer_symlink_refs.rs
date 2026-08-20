//! `core.preferSymlinkRefs` — symbolic references stored as symbolic *links*.
//!
//! git's files backend reads the key when the ref store is created
//! (refs/files-backend.c:129) and, for an update that sets a symbolic target,
//! replaces the `ref: <name>` file with a `symlink(2)` to the target
//! (`create_ref_symlink`, refs/files-backend.c:2094-2119). Reading goes the other
//! way: `read_ref_internal` (:502-538) `lstat`s a reference first and, for a link
//! whose target is a valid `refs/…` name, reports a symref without touching the
//! filesystem's own link resolution.
//!
//! Every expectation here was taken from git 2.55.0 on the same fixture and is
//! asserted as a literal, so the test runs headless with nothing on `PATH` but the
//! binary under test. Skipped on platforms that cannot create a symlink at all.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// git's `die()` exit status.
const FATAL: i32 = 128;

/// The block `create_ref_symlink()` prints, verbatim from git 2.55.0's stderr.
const DEPRECATION: &str = "\
warning: 'core.preferSymlinkRefs=true' is nominated for removal.
hint: The use of symbolic links for symbolic refs is deprecated
hint: and will be removed in Git 3.0. The configuration that
hint: tells Git to use them is thus going away. You can unset
hint: it with:
hint:
hint:\tgit config unset core.preferSymlinkRefs
hint:
hint: Git will then use the textual symref format instead.
";

fn run(repo: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("ZVCS_HOME", home)
        .env("LC_ALL", "C")
        .output()
        .unwrap()
}

fn ok(repo: &Path, home: &Path, args: &[&str]) -> Output {
    let out = run(repo, home, args);
    assert!(
        out.status.success(),
        "setup `git {args:?}` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-symlinkrefs-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::fs::create_dir_all(root.join("repo")).unwrap();
    let root = root.canonicalize().unwrap();
    let (home, repo) = (root.join("home"), root.join("repo"));
    std::fs::write(repo.join("f"), "a\n").unwrap();
    ok(&repo, &home, &["init", "-q", "-b", "main"]);
    ok(&repo, &home, &["config", "user.email", "alice@example.com"]);
    ok(&repo, &home, &["config", "user.name", "Alice"]);
    ok(&repo, &home, &["add", "f"]);
    ok(&repo, &home, &["commit", "-q", "-m", "c0"]);
    (repo, home)
}

/// Whether this filesystem lets us create a symlink at all — the fixtures below
/// are meaningless without one (Windows without developer mode, some CI mounts).
fn symlinks_work(scratch: &Path) -> bool {
    let link = scratch.join("zvcs-symlink-probe");
    let _ = std::fs::remove_file(&link);
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink("target", &link);
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_file("target", &link);
    let worked = made.is_ok();
    let _ = std::fs::remove_file(&link);
    worked
}

#[test]
fn symbolic_ref_writes_a_symlink_and_prints_gits_deprecation_notice() {
    let (repo, home) = fixture("write");
    if !symlinks_work(&repo) {
        return;
    }

    let out = run(
        &repo,
        &home,
        &["-c", "core.preferSymlinkRefs=true", "symbolic-ref", "HEAD", "refs/heads/other"],
    );
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert_eq!(stderr(&out), DEPRECATION);

    let head = repo.join(".git/HEAD");
    assert!(
        head.symlink_metadata().unwrap().file_type().is_symlink(),
        ".git/HEAD should be a symlink"
    );
    assert_eq!(std::fs::read_link(&head).unwrap(), Path::new("refs/heads/other"));

    // …and it reads back as a symbolic ref, not as whatever the link resolves to.
    // `refs/heads/other` does not exist yet, which is the case that used to make
    // repository discovery fail outright.
    let out = ok(&repo, &home, &["symbolic-ref", "HEAD"]);
    assert_eq!(stdout(&out), "refs/heads/other\n");
    let out = ok(&repo, &home, &["status", "-sb"]);
    assert!(
        stdout(&out).starts_with("## No commits yet on other"),
        "unexpected status: {}",
        stdout(&out)
    );
}

#[test]
fn a_symlinked_head_resolves_like_a_textual_one() {
    let (repo, home) = fixture("read");
    if !symlinks_work(&repo) {
        return;
    }

    // Written the way stock git writes it, without going through this port's
    // writer: a bare symlink in place of `.git/HEAD`.
    let head = repo.join(".git/HEAD");
    std::fs::remove_file(&head).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("refs/heads/main", &head).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_file("refs/heads/main", &head).unwrap();

    assert_eq!(stdout(&ok(&repo, &home, &["symbolic-ref", "HEAD"])), "refs/heads/main\n");
    assert_eq!(stdout(&ok(&repo, &home, &["rev-parse", "--abbrev-ref", "HEAD"])), "main\n");
    let log = stdout(&ok(&repo, &home, &["log", "--oneline"]));
    assert!(log.contains("c0"), "unexpected log: {log}");
    assert_eq!(stdout(&ok(&repo, &home, &["status", "--porcelain"])), "");
}

#[test]
fn a_symlink_that_does_not_name_a_reference_is_followed_as_a_file() {
    let (repo, home) = fixture("not-a-ref");
    if !symlinks_work(&repo) {
        return;
    }

    // git only treats a link as a symref when the target starts with `refs/` and
    // is a valid refname (refs/files-backend.c:527-533); anything else falls
    // through to "read whatever it points to". Here `.git/HEAD` points at a plain
    // file holding a textual symref, which must still resolve.
    std::fs::write(repo.join(".git/real-head"), "ref: refs/heads/main\n").unwrap();
    let head = repo.join(".git/HEAD");
    std::fs::remove_file(&head).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("real-head", &head).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_file("real-head", &head).unwrap();

    assert_eq!(stdout(&ok(&repo, &home, &["symbolic-ref", "HEAD"])), "refs/heads/main\n");
}

#[test]
fn the_key_is_off_by_default_and_a_bad_value_refuses_the_update() {
    let (repo, home) = fixture("default-off");

    // Unset: the textual form, no warning.
    let out = ok(&repo, &home, &["symbolic-ref", "HEAD", "refs/heads/other"]);
    assert_eq!(stderr(&out), "");
    assert_eq!(
        std::fs::read_to_string(repo.join(".git/HEAD")).unwrap(),
        "ref: refs/heads/other\n"
    );

    // Explicitly false: still textual.
    let out = ok(
        &repo,
        &home,
        &["-c", "core.preferSymlinkRefs=false", "symbolic-ref", "HEAD", "refs/heads/main"],
    );
    assert_eq!(stderr(&out), "");
    assert!(!repo.join(".git/HEAD").symlink_metadata().unwrap().file_type().is_symlink());

    // Unreadable: git dies while creating the ref store, before any update.
    let out = run(
        &repo,
        &home,
        &["-c", "core.preferSymlinkRefs=bogus", "symbolic-ref", "HEAD", "refs/heads/third"],
    );
    assert_eq!(
        stderr(&out),
        "fatal: bad boolean config value 'bogus' for 'core.prefersymlinkrefs'\n"
    );
    assert_eq!(out.status.code().unwrap_or(-1), FATAL);
    // …and HEAD is untouched.
    assert_eq!(
        std::fs::read_to_string(repo.join(".git/HEAD")).unwrap(),
        "ref: refs/heads/main\n"
    );
}
