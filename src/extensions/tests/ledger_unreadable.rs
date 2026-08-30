//! What the read verbs say when the shared ledger cannot be read.
//!
//! Three states have to stay distinguishable: no ledger yet (a machine that has
//! never indexed anything — an empty answer is the truth), a ledger that reads
//! (the answer), and a ledger that exists but cannot be opened (not an answer at
//! all). The middle and last were reported identically: `git zsnapshots`,
//! `git zstashes`, `git zrepos`, `git zwho` and `git zjobs` all printed nothing
//! and exited 0 over an unreadable store, and every `[selectors]` verb reported
//! "no repos matched" over a tree it simply could not read.
//!
//! Someone checking whether they have a restore point before doing something
//! destructive is told they have none. That is the worst way for this to be
//! wrong, which is why an error is now the answer.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
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

fn out_of(o: &Output) -> String {
    let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&o.stderr));
    s
}

/// A repo with a snapshot and an index entry, so every listing below has
/// something real to fail to report.
fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-ledgerro-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    run(&repo, &home, &["init", "-q", "-b", "main"]);
    run(&repo, &home, &["config", "user.email", "t@example"]);
    run(&repo, &home, &["config", "user.name", "T"]);
    run(&repo, &home, &["commit", "-q", "--allow-empty", "-m", "c0"]);
    run(&repo, &home, &["zsnapshot", "restore-point"]);
    run(&repo, &home, &["zreindex", "--sync", repo.to_str().unwrap()]);
    (root, home, repo)
}

/// Make the ledger unreadable, and report whether that actually took effect —
/// a process that ignores file permissions (root in a container) cannot run
/// these cases, and a test that silently passes there is worse than one that
/// says why it stopped.
fn make_unreadable(home: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let db = home.join("db.sqlite");
    assert!(db.exists(), "precondition: the ledger exists");
    std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o000)).unwrap();
    std::fs::read(&db).is_err()
}

fn make_readable(home: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(home.join("db.sqlite"), std::fs::Permissions::from_mode(0o644));
}

/// The listings whose empty output is indistinguishable from "nothing there".
const LISTINGS: &[&[&str]] = &[
    &["zsnapshots"],
    &["zstashes"],
    &["zrepos"],
    &["zwho"],
    &["zjobs"],
];

#[test]
fn an_unreadable_ledger_is_an_error_not_an_empty_listing() {
    let (root, home, repo) = fixture("listings");

    // Control: with a readable ledger the snapshot is listed, so the failure
    // below is about readability and not about an empty store.
    let listed = out_of(&run(&repo, &home, &["zsnapshots"]));
    assert!(listed.contains("restore-point"), "precondition: the snapshot is listed:\n{listed}");

    if !make_unreadable(&home) {
        eprintln!("skipping: this process can read a 0o000 file (running as root?)");
        let _ = std::fs::remove_dir_all(&root);
        return;
    }

    for args in LISTINGS {
        let o = run(&repo, &home, args);
        let text = out_of(&o);
        assert!(
            !o.status.success(),
            "`git {}` reported success over an unreadable ledger:\n{text}",
            args.join(" ")
        );
        assert!(
            text.contains("open db"),
            "`git {}` must say it could not open the ledger:\n{text}",
            args.join(" ")
        );
    }

    make_readable(&home);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_fleet_verb_does_not_report_an_empty_tree_it_cannot_read() {
    let (root, home, repo) = fixture("selector");

    // The selector resolves every `[selectors]` verb's repo set from the ledger.
    // Reading "no repos matched" from a tree that could not be read would send a
    // fan-out over nothing while reporting a clean run.
    let listed = out_of(&run(&repo, &home, &["zheads"]));
    assert!(listed.contains("repo"), "precondition: the fixture repo is indexed:\n{listed}");

    if !make_unreadable(&home) {
        eprintln!("skipping: this process can read a 0o000 file (running as root?)");
        let _ = std::fs::remove_dir_all(&root);
        return;
    }

    for verb in ["zheads", "zdirty", "zgc"] {
        let o = run(&repo, &home, &[verb]);
        let text = out_of(&o);
        assert!(!o.status.success(), "`git {verb}` reported success over an unreadable ledger:\n{text}");
        assert!(
            !text.contains("no repos matched"),
            "`git {verb}` reported an empty selection over an unreadable ledger:\n{text}"
        );
    }

    make_readable(&home);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_ledger_that_was_never_created_stays_quiet() {
    // The other half of the contract: a machine that has never indexed anything
    // must not be told its store is broken. This is why the check is for the
    // file's presence rather than for any open error.
    let root = std::env::temp_dir().join(format!("zvcs-ledgerro-fresh-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    run(&repo, &home, &["init", "-q", "-b", "main"]);
    assert!(!home.join("db.sqlite").exists(), "precondition: no ledger yet");

    for args in [&["zsnapshots"][..], &["zrepos"], &["zjobs"], &["zheads"]] {
        let o = run(&repo, &home, args);
        assert!(
            o.status.success(),
            "`git {}` must succeed with no ledger yet:\n{}",
            args.join(" "),
            out_of(&o)
        );
    }

    let _ = std::fs::remove_dir_all(&root);
}
