//! `git zreview` — the screen you read before committing across a tree.
//!
//! It prints each repository's `git status --short` block with a diffstat
//! summary, omits clean repositories, and ends with a count. The verb had no
//! test of its own, and its collector answered `None` for two different things:
//! a repository that is clean, and one whose status probe did not run at all.
//! Both were skipped, so a repository holding uncommitted work vanished from
//! the one screen whose job is to show uncommitted work, and the summary
//! counted it among "indexed" without a word. Measured on three repositories
//! each with a modification, two of them made unreadable: `zreview: 1 repo(s)
//! with 1 pending change(s) across 3 indexed`.
//!
//! What the verb gets right is pinned alongside, since nothing pinned it
//! before: the per-repo block, the omission of clean repositories, and the
//! counts.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(home: &Path, dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap()
}

fn ok(home: &Path, dir: &Path, args: &[&str]) -> String {
    let out = run(home, dir, args);
    assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// stdout and stderr together, with colour stripped: the blocks are bold and
/// the diffstat dim, and neither should have to be matched around.
fn screen(home: &Path, dir: &Path, args: &[&str]) -> String {
    let out = run(home, dir, args);
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    let mut plain = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            plain.push(c);
        }
    }
    plain
}

fn init_repo(home: &Path, at: &Path) {
    std::fs::create_dir_all(at).unwrap();
    ok(home, at, &["init", "-q", "-b", "main"]);
    ok(home, at, &["config", "user.email", "t@example"]);
    ok(home, at, &["config", "user.name", "T"]);
    std::fs::write(at.join("f.txt"), b"v\n").unwrap();
    ok(home, at, &["add", "f.txt"]);
    ok(home, at, &["commit", "-q", "-m", "c0"]);
}

#[test]
fn only_repositories_with_pending_work_are_shown() {
    let root = std::env::temp_dir().join(format!("zvcs-zreview-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    for name in ["modified", "clean", "staged"] {
        init_repo(&home, &root.join(name));
    }
    // One unstaged edit, one staged addition, one untouched.
    std::fs::write(root.join("modified/f.txt"), b"changed\n").unwrap();
    std::fs::write(root.join("staged/g.txt"), b"new\n").unwrap();
    ok(&home, &root.join("staged"), &["add", "g.txt"]);
    ok(&home, &root, &["zreindex", "--sync", root.to_str().unwrap()]);

    let out = screen(&home, &root, &["zreview"]);
    assert!(out.contains("== ") && out.contains("/modified"), "the modified repo must have a block:\n{out}");
    assert!(out.contains("/staged"), "the staged repo must have a block:\n{out}");
    assert!(!out.contains("/clean"), "a clean repository must be omitted:\n{out}");
    assert!(out.contains(" M f.txt"), "the status block must be shown verbatim:\n{out}");
    assert!(out.contains("A  g.txt"), "a staged addition must be shown:\n{out}");
    assert!(out.contains("1 file changed"), "the diffstat summary line must be shown:\n{out}");
    assert!(
        out.contains("2 repo(s) with 2 pending change(s) across 3 indexed"),
        "the summary must count repositories and entries:\n{out}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_repository_that_cannot_be_read_is_shown_not_skipped() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("zvcs-zreview-unread-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    for name in ["visible", "gone", "locked"] {
        init_repo(&home, &root.join(name));
        // Every one of them has work pending, so anything missing from the
        // screen is work that would go unreviewed.
        std::fs::write(root.join(name).join("f.txt"), b"changed\n").unwrap();
    }
    ok(&home, &root, &["zreindex", "--sync", root.to_str().unwrap()]);

    std::fs::remove_dir_all(root.join("gone/.git")).unwrap();
    let locked = root.join("locked/.git");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_dir(&locked).is_ok() {
        eprintln!("skipping: this process can read a 0o000 directory (running as root?)");
        let _ = std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755));
        let _ = std::fs::remove_dir_all(&root);
        return;
    }

    let out = screen(&home, &root, &["zreview"]);
    assert!(out.contains("/visible"), "the readable repository must still be reviewed:\n{out}");
    assert_eq!(
        out.matches("(unreadable)").count(),
        2,
        "both unreadable repositories must appear on the screen:\n{out}"
    );
    assert!(out.contains("2 unreadable"), "the summary must disclose them:\n{out}");
    assert!(
        out.contains("1 repo(s) with 1 pending change(s)"),
        "only what was actually reviewed may be counted as pending:\n{out}"
    );

    let _ = std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_tree_with_nothing_pending_says_so_without_a_note() {
    let root = std::env::temp_dir().join(format!("zvcs-zreview-quiet-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    init_repo(&home, &root.join("only"));
    ok(&home, &root, &["zreindex", "--sync", root.to_str().unwrap()]);

    let out = screen(&home, &root, &["zreview"]);
    assert!(out.contains("0 repo(s) with 0 pending change(s)"), "a clean tree must report nothing pending:\n{out}");
    assert!(!out.contains("unreadable"), "a healthy tree must not mention unreadable repositories:\n{out}");

    let _ = std::fs::remove_dir_all(&root);
}
