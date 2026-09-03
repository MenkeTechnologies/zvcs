//! What a fleet read says about a repository it could not open.
//!
//! The read verbs open every selected repository and map a failure to a value:
//! `probe(gd, f, |_| 0)`, `|_| false`, `|_| None`. A value is an answer, so a
//! repository nobody can read was reported as **clean** by `zdirty`, as `0
//! commit(s)` by `zcommits`, as `0 tag(s)` by `ztags` — and counted in the
//! totals as though it had been read. Measured on three repositories with two
//! made unreadable: `zcommits: 1 commits across 3 repos`, with nothing saying
//! that two of the three were never opened.
//!
//! `zheads` always had the right shape — it prints `(open failed: …)` against
//! the repository — and these three now match it: the row says the repository
//! could not be read, the totals count only what was read, and the summary line
//! says how many were skipped. An incomplete answer that says so is usable; one
//! that looks complete is not.
//!
//! Two ways of being unopenable are covered, because they fail at different
//! layers: a git dir that has been removed, and one whose permissions deny it.

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

/// stdout and stderr: the rows are on one, the summary on the other.
fn both(home: &Path, dir: &Path, args: &[&str]) -> String {
    let out = run(home, dir, args);
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    s
}

/// Three indexed repositories: one readable and dirty, one whose git dir is
/// removed, one whose git dir is unreadable. Returns `None` when this process
/// can read a 0o000 directory anyway (root in a container), since half the
/// fixture would then not be what it claims.
fn fixture(tag: &str) -> Option<(PathBuf, PathBuf)> {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("zvcs-unread-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();

    for name in ["good", "gone", "locked"] {
        let r = root.join(name);
        std::fs::create_dir_all(&r).unwrap();
        ok(&home, &r, &["init", "-q", "-b", "main"]);
        ok(&home, &r, &["config", "user.email", "t@example"]);
        ok(&home, &r, &["config", "user.name", "T"]);
        std::fs::write(r.join("f.txt"), b"v\n").unwrap();
        ok(&home, &r, &["add", "f.txt"]);
        ok(&home, &r, &["commit", "-q", "-m", "c0"]);
    }
    // The readable one is dirty, so `zdirty` has a true positive to report
    // alongside the two it cannot judge.
    std::fs::write(root.join("good/f.txt"), b"changed\n").unwrap();
    ok(&home, &root, &["zreindex", "--sync", root.to_str().unwrap()]);

    std::fs::remove_dir_all(root.join("gone/.git")).unwrap();
    let locked = root.join("locked/.git");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_dir(&locked).is_ok() {
        let _ = std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755));
        let _ = std::fs::remove_dir_all(&root);
        return None;
    }
    Some((root, home))
}

fn restore(root: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(root.join("locked/.git"), std::fs::Permissions::from_mode(0o755));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn zdirty_does_not_call_an_unreadable_repository_clean() {
    let Some((root, home)) = fixture("dirty") else {
        eprintln!("skipping: this process can read a 0o000 directory (running as root?)");
        return;
    };

    // The dirty one is listed; the two that could not be opened are not listed
    // as clean by omission — the summary says they were skipped.
    let out = both(&home, &root, &["zdirty"]);
    assert!(out.contains("/good"), "the dirty repository must be listed:\n{out}");
    assert!(out.contains("2 unreadable"), "zdirty must say it could not read two repositories:\n{out}");
    assert!(out.contains("1 dirty of 3 indexed"), "the counts must still describe the whole selection:\n{out}");

    restore(&root);
}

#[test]
fn counts_exclude_repositories_that_were_never_opened() {
    let Some((root, home)) = fixture("counts") else {
        eprintln!("skipping: this process can read a 0o000 directory (running as root?)");
        return;
    };

    // `zcommits`: one real count, two rows that say why there is no number, and
    // a total over what was actually read.
    let commits = both(&home, &root, &["zcommits"]);
    assert!(commits.contains("1 commit(s)"), "the readable repository must be counted:\n{commits}");
    assert_eq!(
        commits.matches("(unreadable)").count(),
        2,
        "both unopenable repositories must say so rather than showing 0:\n{commits}"
    );
    assert!(commits.contains("2 unreadable"), "the summary must disclose the skipped repositories:\n{commits}");

    // `ztags` the same way: a repository with no tags and one that could not be
    // read must not print the same thing.
    let tags = both(&home, &root, &["ztags"]);
    assert!(tags.contains("0 tag(s)"), "the readable repository has no tags and should say so:\n{tags}");
    assert_eq!(tags.matches("(unreadable)").count(), 2, "unreadable is not zero:\n{tags}");

    restore(&root);
}

#[test]
fn a_fleet_that_reads_completely_says_nothing_about_unreadable_repositories() {
    // The other half of the contract: the note only appears when it is true, so
    // it stays meaningful. Nothing is broken in this fixture.
    let root = std::env::temp_dir().join(format!("zvcs-unread-clean-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let r = root.join("only");
    std::fs::create_dir_all(&r).unwrap();
    ok(&home, &r, &["init", "-q", "-b", "main"]);
    ok(&home, &r, &["config", "user.email", "t@example"]);
    ok(&home, &r, &["config", "user.name", "T"]);
    ok(&home, &r, &["commit", "-q", "--allow-empty", "-m", "c0"]);
    ok(&home, &root, &["zreindex", "--sync", root.to_str().unwrap()]);

    for verb in ["zdirty", "zcommits", "ztags"] {
        let out = both(&home, &root, &[verb]);
        assert!(!out.contains("unreadable"), "`git {verb}` reported an unreadable repository in a healthy fleet:\n{out}");
    }

    let _ = std::fs::remove_dir_all(&root);
}
