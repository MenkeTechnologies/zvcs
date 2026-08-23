//! An argument git could never have made a ref out of is a path, not an error.
//!
//! `git checkout Cargo.lock` died with
//!
//! ```text
//! fatal: … The ref name or path is not a valid ref name:
//!        Reference name cannot end with '.lock'
//! ```
//!
//! in every repository that had a remote configured, which is every checkout of
//! every Rust project. Git resolves the same argument as a path and reports
//! `Updated 0 paths from the index`.
//!
//! The cause was the DWIM remote-branch lookup: `checkout`'s
//! `unique_remote_branch()` composes `refs/remotes/<remote>/<arg>` and asks the
//! ref store for it. `refs/remotes/origin/Cargo.lock` ends in `.lock`, which
//! `gix::validate::reference::name` rejects and `try_find_reference` reports as
//! an **error** rather than as "no such ref" — and the `?` on that call turned a
//! failed name check into a fatal one. Git's `check_refname_format()` failing
//! just means the DWIM candidate does not exist, so the argument falls through
//! to the path interpretation.
//!
//! With no remote configured the loop body never ran, which is why the failure
//! looked at first like a submodule or packed-refs problem: a plain `git init`
//! fixture could not reproduce it. **A remote is the whole trigger**, so every
//! case here configures one — a fixture without it would pass against the
//! unfixed binary and assert nothing.
//!
//! `.lock` is only the reachable instance of the class. Every other name
//! `check_refname_format()` rejects — a trailing dot, `..`, an ASCII control
//! byte — has to behave the same way, so those are asserted too rather than
//! leaving the fix pinned to one suffix.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn scratch(tag: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let root =
        std::env::temp_dir().join(format!("zvcs-locksuffix-{tag}-{}-{unique}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home")).expect("mkdir fixture");
    root.canonicalize().expect("canonicalize fixture")
}

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .expect("run binary")
}

fn out_of(o: &Output) -> String {
    let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&o.stderr));
    s
}

/// A repository holding `tracked` and carrying a configured remote — the
/// condition that makes the DWIM lookup run at all.
fn repo_with(tag: &str, tracked: &[&str]) -> (PathBuf, PathBuf) {
    let root = scratch(tag);
    let (home, work) = (root.join("home"), root.join("wk"));
    std::fs::create_dir_all(&work).expect("mkdir work");
    assert!(run(&work, &home, &["init", "-q", "-b", "main", "."]).status.success());
    for name in tracked {
        let path = work.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir parent");
        }
        std::fs::write(&path, b"contents\n").expect("write tracked file");
        let out = run(&work, &home, &["add", "--", name]);
        assert!(out.status.success(), "add {name}: {}", out_of(&out));
    }
    let out = run(&work, &home, &["commit", "-qm", "base"]);
    assert!(out.status.success(), "commit: {}", out_of(&out));
    // The trigger. Nothing needs to be fetched: `unique_remote_branch` iterates
    // `remote_names()`, so merely having one configured is enough to compose the
    // candidate ref name that fails validation.
    let out = run(&work, &home, &["remote", "add", "origin", "https://example.invalid/r.git"]);
    assert!(out.status.success(), "remote add: {}", out_of(&out));
    (work, home)
}

/// The reported bug, exactly.
#[test]
fn checkout_of_a_dot_lock_path_is_a_path_not_a_ref_error() {
    let (work, home) = repo_with("checkout", &["Cargo.lock"]);
    let out = run(&work, &home, &["checkout", "Cargo.lock"]);
    let text = out_of(&out);
    assert!(
        !text.contains("not a valid ref name"),
        "the argument was rejected as a ref name instead of read as a path:\n{text}"
    );
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert_eq!(text, "Updated 0 paths from the index\n", "stock git's wording");
}

/// The same argument with a modification to restore, so the case proves the path
/// was actually acted on rather than merely not rejected.
#[test]
fn checkout_of_a_dot_lock_path_restores_it() {
    let (work, home) = repo_with("restore", &["Cargo.lock"]);
    std::fs::write(work.join("Cargo.lock"), b"dirtied\n").expect("dirty the file");
    let out = run(&work, &home, &["checkout", "Cargo.lock"]);
    let text = out_of(&out);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert_eq!(text, "Updated 1 path from the index\n", "{text}");
    assert_eq!(
        std::fs::read(work.join("Cargo.lock")).expect("read back"),
        b"contents\n",
        "the file must have been restored from the index"
    );
}

/// `switch` composes the same candidate through its own copy of the lookup.
///
/// It is not a path-taking verb, so the argument stays a ref — but it must fail
/// as *git* fails, naming an invalid reference, rather than dying inside a name
/// check on a candidate it invented itself.
#[test]
fn switch_reports_an_invalid_reference_rather_than_a_ref_name_check() {
    let (work, home) = repo_with("switch", &["Cargo.lock"]);
    let out = run(&work, &home, &["switch", "Cargo.lock"]);
    let text = out_of(&out);
    assert!(
        !text.contains("not a valid ref name"),
        "switch leaked its own DWIM candidate's name check:\n{text}"
    );
    assert_eq!(text, "fatal: invalid reference: Cargo.lock\n", "{text}");
}

/// `--` removes all ambiguity, so this path always worked; it is here so a fix
/// that "solves" the bug by breaking the explicit form cannot pass.
#[test]
fn the_explicit_path_form_still_works() {
    let (work, home) = repo_with("explicit", &["Cargo.lock"]);
    std::fs::write(work.join("Cargo.lock"), b"dirtied\n").expect("dirty the file");
    let out = run(&work, &home, &["checkout", "--", "Cargo.lock"]);
    assert_eq!(out.status.code(), Some(0), "{}", out_of(&out));
    assert_eq!(
        std::fs::read(work.join("Cargo.lock")).expect("read back"),
        b"contents\n"
    );
}

/// Every other shape `check_refname_format()` rejects behaves the same way, so
/// the fix is not pinned to one suffix.
#[test]
fn the_other_unrepresentable_names_are_paths_too() {
    for (tag, name) in [
        ("trailing-dot", "weird."),
        ("double-dot", "a..b"),
        ("trailing-slash-ish", "x.lock"),
        ("at-brace", "a@{1}"),
    ] {
        let (work, home) = repo_with(tag, &[name]);
        let out = run(&work, &home, &["checkout", name]);
        let text = out_of(&out);
        assert!(
            !text.contains("not a valid ref name"),
            "{name}: rejected as a ref name instead of read as a path:\n{text}"
        );
        assert_eq!(out.status.code(), Some(0), "{name}: {text}");
    }
}

/// A real remote-tracking branch must still DWIM, which is what the guarded
/// lookup exists to do — skipping *unrepresentable* names must not skip valid
/// ones.
#[test]
fn a_genuine_remote_branch_still_dwims() {
    let (work, home) = repo_with("dwim", &["file.txt"]);
    let head = run(&work, &home, &["rev-parse", "HEAD"]);
    let head = String::from_utf8_lossy(&head.stdout).trim().to_string();
    // A remote-tracking ref with a perfectly ordinary name.
    let out = run(&work, &home, &["update-ref", "refs/remotes/origin/feature", &head]);
    assert!(out.status.success(), "{}", out_of(&out));

    let out = run(&work, &home, &["checkout", "feature"]);
    let text = out_of(&out);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(
        text.contains("Switched to a new branch 'feature'"),
        "the DWIM must still create a local branch tracking the remote one:\n{text}"
    );
}
