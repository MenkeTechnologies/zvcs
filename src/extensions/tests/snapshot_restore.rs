//! `git zsnapshot` / `git zrestore` / `git zsnapshots` — a named restore point
//! for a whole submodule tree.
//!
//! `zrestore` is destructive by design (it is a `reset --hard` per repo), so the
//! contract worth pinning is exactly where the destruction stops: it must reach
//! every nested submodule, it must leave untracked files alone, it must refuse a
//! name it does not know without touching anything, and it must report a partial
//! restore as a failure rather than a success.
//!
//! One consequence is easy to meet by accident and is pinned here deliberately:
//! a snapshot stores commit ids, not branch names, so a restore moves whichever
//! branch is checked out *now*. Snapshot on `main`, branch off, restore, and the
//! new branch is reset onto the old commit with its own tip left only in the
//! reflog.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap()
}

fn git(home: &Path, cwd: &Path, args: &[&str]) {
    let out = run(home, cwd, args);
    assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
}

fn stdout(home: &Path, cwd: &Path, args: &[&str]) -> String {
    String::from_utf8_lossy(&run(home, cwd, args).stdout).trim().to_string()
}

fn head(home: &Path, repo: &Path) -> String {
    stdout(home, repo, &["rev-parse", "HEAD"])
}

/// A repo with one commit, configured so commits need no ambient identity.
fn init(home: &Path, at: &Path, file: &str) {
    std::fs::create_dir_all(at).unwrap();
    git(home, at, &["init", "-q", "-b", "main"]);
    git(home, at, &["config", "user.email", "t@example"]);
    git(home, at, &["config", "user.name", "T"]);
    std::fs::write(at.join(file), b"1\n").unwrap();
    git(home, at, &["add", file]);
    git(home, at, &["commit", "-q", "-m", "c0"]);
}

/// `super` → `sub` → `deep`: two levels of nesting, so a `collect` that walked
/// only the first level of submodules is caught.
fn nested_tree(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-snap-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");

    init(&home, &root.join("origin-deep"), "d.txt");
    init(&home, &root.join("origin-sub"), "s.txt");
    // `deep` inside `sub`, then `sub` inside `super` — each added from a local
    // path, which git only allows with protocol.file.allow.
    let allow = ["-c", "protocol.file.allow=always"];
    let sub = root.join("origin-sub");
    git(&home, &sub, &[&allow[..], &["submodule", "add", "-q", root.join("origin-deep").to_str().unwrap(), "deep"]].concat());
    git(&home, &sub, &["commit", "-q", "-m", "add deep"]);

    let top = root.join("super");
    init(&home, &top, "p.txt");
    git(&home, &top, &[&allow[..], &["submodule", "add", "-q", sub.to_str().unwrap(), "sub"]].concat());
    git(&home, &top, &["commit", "-q", "-m", "add sub"]);
    git(&home, &top, &[&allow[..], &["submodule", "update", "--init", "--recursive"]].concat());

    (root, home, top)
}

#[test]
fn restore_reaches_every_nested_submodule_and_spares_untracked_files() {
    let (root, home, top) = nested_tree("nested");
    let sub = top.join("sub");
    let deep = sub.join("deep");
    assert!(deep.join(".git").exists(), "precondition: the nested submodule is checked out");

    let snap = run(&home, &top, &["zsnapshot", "base"]);
    let note = String::from_utf8_lossy(&snap.stdout).into_owned();
    assert!(note.contains("3 repo(s)"), "all three repos must be captured, not just the top:\n{note}");
    let (was_top, was_sub, was_deep) = (head(&home, &top), head(&home, &sub), head(&home, &deep));

    // Move all three forward.
    for (repo, file) in [(&top, "p.txt"), (&sub, "s.txt"), (&deep, "d.txt")] {
        std::fs::write(repo.join(file), b"2\n").unwrap();
        git(&home, repo, &["commit", "-q", "-am", "advance"]);
    }
    assert_ne!(head(&home, &deep), was_deep, "precondition: the deepest repo moved");

    // Untracked files in each repo must survive a restore — the module documents
    // `reset --hard`, which spares them, and losing them would be silent.
    for repo in [&top, &sub, &deep] {
        std::fs::write(repo.join("scratch.txt"), b"keep me\n").unwrap();
    }

    let out = run(&home, &top, &["zrestore", "base"]);
    assert!(out.status.success(), "restore must succeed: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(head(&home, &top), was_top, "the superproject was not restored");
    assert_eq!(head(&home, &sub), was_sub, "the submodule was not restored");
    assert_eq!(head(&home, &deep), was_deep, "the NESTED submodule was not restored");
    for repo in [&top, &sub, &deep] {
        assert!(repo.join("scratch.txt").exists(), "restore deleted an untracked file in {}", repo.display());
    }

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_unknown_name_fails_without_touching_the_tree() {
    let (root, home, top) = nested_tree("unknown");
    let before = head(&home, &top);

    let out = run(&home, &top, &["zrestore", "no-such-snapshot"]);
    assert!(!out.status.success(), "an unknown snapshot must fail");
    let msg = String::from_utf8_lossy(&out.stderr);
    assert!(msg.contains("no snapshot named"), "the error must name the problem:\n{msg}");
    assert_eq!(head(&home, &top), before, "a failed lookup must not move HEAD");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn resnapshotting_a_name_replaces_it() {
    let (root, home, top) = nested_tree("replace");
    run(&home, &top, &["zsnapshot", "point"]);

    std::fs::write(top.join("p.txt"), b"2\n").unwrap();
    git(&home, &top, &["commit", "-q", "-am", "second"]);
    let second = head(&home, &top);
    run(&home, &top, &["zsnapshot", "point"]);

    // The listing counts rows per name: a re-save that appended instead of
    // replacing would double them, and a restore would then reset twice.
    let list = stdout(&home, &top, &["zsnapshots"]);
    let row = list.lines().find(|l| l.starts_with("point")).unwrap_or_default().to_string();
    assert_eq!(row, "point\t3", "re-snapshotting must replace the name's rows, not add to them:\n{list}");

    // And the newer commit is what comes back.
    std::fs::write(top.join("p.txt"), b"3\n").unwrap();
    git(&home, &top, &["commit", "-q", "-am", "third"]);
    run(&home, &top, &["zrestore", "point"]);
    assert_eq!(head(&home, &top), second, "restore must use the latest save of the name");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_partial_restore_is_reported_as_a_failure() {
    let (root, home, top) = nested_tree("partial");
    run(&home, &top, &["zsnapshot", "base"]);

    // Take one recorded repo out from under the restore. Each repo is reset
    // independently, so the run must finish the others and still fail overall —
    // a half-restored tree reported as success is the worst outcome here.
    std::fs::rename(top.join("sub").join("deep"), top.join("sub").join("deep-moved")).unwrap();

    let out = run(&home, &top, &["zrestore", "base"]);
    let said = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(!out.status.success(), "a partial restore must exit non-zero:\n{said}");
    assert!(said.contains("1 failed"), "the summary must count the failure:\n{said}");
    assert!(said.contains("restored 2 repo(s)"), "the reachable repos must still be restored:\n{said}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn restore_moves_the_branch_that_is_checked_out_now() {
    // A snapshot stores commit ids, not branch names. Restoring while on a
    // different branch resets THAT branch onto the snapshot's commit; the branch
    // it was pointing at survives only in the reflog. Pinned so the semantic
    // cannot change quietly under a destructive verb.
    let (root, home, top) = nested_tree("branch");
    run(&home, &top, &["zsnapshot", "base"]);
    let snapped = head(&home, &top);

    git(&home, &top, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(top.join("p.txt"), b"feature\n").unwrap();
    git(&home, &top, &["commit", "-q", "-am", "feature work"]);
    let feature_tip = head(&home, &top);

    run(&home, &top, &["zrestore", "base"]);

    assert_eq!(stdout(&home, &top, &["rev-parse", "--abbrev-ref", "HEAD"]), "feature",
        "restore must not switch branches");
    assert_eq!(head(&home, &top), snapped, "the checked-out branch is reset onto the snapshot");
    assert_eq!(stdout(&home, &top, &["rev-parse", "main"]), snapped, "main was already there and stays");

    // The displaced tip is unreachable from any branch but still recoverable.
    let contains = stdout(&home, &top, &["branch", "--contains", &feature_tip]);
    assert!(contains.is_empty(), "the displaced commit must be off every branch:\n{contains}");
    let reflog = stdout(&home, &top, &["reflog", "show", "feature"]);
    assert!(reflog.contains(&feature_tip[..7]), "the displaced tip must remain in the reflog:\n{reflog}");

    let _ = std::fs::remove_dir_all(&root);
}
