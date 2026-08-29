//! `git zpin` / `git zunpin` — freezing a repository from daemon autonomy.
//!
//! A pin is a promise made to a background process: `watch.rs` reads
//! `repos.pinned` before it reconciles or autobumps, and refuses when the flag
//! is set. The person setting it has usually just decided that this one repo
//! must not move on its own — mid-bisect, mid-review, mid-anything — so a pin
//! that does not stick is worse than no pin at all, because it was believed.
//!
//! Nothing here needs a daemon: the flag is a row in the shared database, and
//! these cases assert that the row is written, survives the process that wrote
//! it, and is cleared exactly when asked. What they cannot assert is the
//! daemon's half — that `react()` honours the flag — which stays with the
//! daemon's own tests.
//!
//! The partial-failure case is the one worth reading twice: `zpin a b` where
//! `a` is not a repository must still pin `b`. A loop that aborts on the first
//! bad argument silently leaves the rest of the tree unfrozen, which is the
//! failure this verb exists to prevent.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home.join("zvcs"))
        .env("ZVCS_SOCK", home.join("sock"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .output()
        .unwrap()
}

fn ok(out: &Output, what: &str) -> String {
    assert!(
        out.status.success(),
        "{what} failed ({}): {}{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn both(out: &Output) -> String {
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    s
}

/// Two repositories, neither indexed yet.
fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-zpin-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    std::fs::create_dir_all(home.join("zvcs")).unwrap();
    let work = root.join("work");
    for name in ["one", "two"] {
        let repo = work.join(name);
        std::fs::create_dir_all(&repo).unwrap();
        ok(&run(&repo, &home, &["init", "-q", "-b", "main", "."]), "init");
        std::fs::write(repo.join("a.txt"), b"x\n").unwrap();
        ok(&run(&repo, &home, &["add", "a.txt"]), "add");
        ok(&run(&repo, &home, &["commit", "-q", "-m", "first"]), "commit");
    }
    (work.join("one"), work.join("two"), home)
}

#[test]
fn a_pin_survives_the_process_that_set_it() {
    // The flag is a database row, not process state: a later, unrelated
    // invocation has to see it, or the daemon never will either.
    let (one, _two, home) = fixture("persist");
    ok(&run(&one, &home, &["zpin"]), "pin cwd");

    let listed = ok(&run(&one, &home, &["zpin", "list"]), "list");
    assert!(listed.contains("one"), "the pin is not listed:\n{listed}");
    // The workdir is what a human recognises, not the git dir.
    assert!(!listed.contains(".git"), "the listing shows the git dir:\n{listed}");

    // Pinning twice leaves one entry rather than two.
    ok(&run(&one, &home, &["zpin"]), "pin again");
    let listed = ok(&run(&one, &home, &["zpin", "list"]), "list again");
    assert_eq!(listed.lines().filter(|l| l.contains("one")).count(), 1, "{listed}");

    let _ = std::fs::remove_dir_all(one.parent().unwrap().parent().unwrap());
}

#[test]
fn pinning_indexes_a_repository_that_was_not_known_yet() {
    // `set_all` upserts the repo before setting the flag, so a pin does not
    // silently land on a row that does not exist. Without that, freezing a repo
    // the crawler had never reached would report success and do nothing.
    let (one, _two, home) = fixture("index");
    // Building the fixture through this binary already indexes the repos —
    // measured, and the reason this case drops the database rather than
    // assuming a fresh one. What is under test is that `zpin` recreates the row
    // it needs, not that the fixture happened to be unknown.
    let db = home.join("zvcs/db.sqlite");
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
    }
    let before = both(&run(&one, &home, &["zrepos"]));
    assert!(!before.contains("/one"), "the index was not cleared:\n{before}");

    ok(&run(&one, &home, &["zpin"]), "pin");
    let after = both(&run(&one, &home, &["zrepos"]));
    assert!(after.contains("one"), "pinning did not index the repository:\n{after}");

    let _ = std::fs::remove_dir_all(one.parent().unwrap().parent().unwrap());
}

#[test]
fn unpin_clears_one_and_all_clears_everything() {
    let (one, two, home) = fixture("clear");
    ok(&run(&one, &home, &["zpin"]), "pin one");
    ok(&run(&two, &home, &["zpin"]), "pin two");
    let listed = ok(&run(&one, &home, &["zpin", "list"]), "list");
    assert!(listed.contains("one") && listed.contains("two"), "{listed}");

    ok(&run(&one, &home, &["zunpin"]), "unpin cwd");
    let listed = ok(&run(&one, &home, &["zpin", "list"]), "list after unpin");
    assert!(!listed.contains("/one"), "the cwd repo is still pinned:\n{listed}");
    assert!(listed.contains("two"), "unpinning one repo cleared another:\n{listed}");

    ok(&run(&one, &home, &["zunpin", "--all"]), "unpin all");
    let listed = ok(&run(&one, &home, &["zpin", "list"]), "list after --all");
    assert!(listed.contains("no pinned repos"), "{listed}");

    let _ = std::fs::remove_dir_all(one.parent().unwrap().parent().unwrap());
}

#[test]
fn a_bad_path_does_not_stop_the_repositories_after_it() {
    // `zpin <bad> <good>` must still freeze the good one. Aborting on the first
    // bad argument leaves the rest of the tree autonomous while the command
    // looks like it did something.
    let (one, two, home) = fixture("partial");
    let missing = one.parent().unwrap().join("not-a-repo");
    std::fs::create_dir_all(&missing).unwrap();

    let out = run(
        &one,
        &home,
        &["zpin", missing.to_str().unwrap(), two.to_str().unwrap()],
    );
    let text = both(&out);
    assert!(text.contains("not a git repository"), "the bad path was not reported:\n{text}");

    let listed = ok(&run(&one, &home, &["zpin", "list"]), "list");
    assert!(listed.contains("two"), "the repository after the bad one was not pinned:\n{listed}");

    let _ = std::fs::remove_dir_all(one.parent().unwrap().parent().unwrap());
}

#[test]
fn the_json_listing_is_one_object_per_pinned_repo() {
    let (one, two, home) = fixture("json");
    ok(&run(&one, &home, &["zpin"]), "pin one");
    ok(&run(&two, &home, &["zpin"]), "pin two");

    let out = ok(&run(&one, &home, &["zpin", "list", "--json"]), "list --json");
    let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 2, "expected one object per pin:\n{out}");
    for l in &lines {
        assert!(l.starts_with('{') && l.ends_with('}'), "not one object per line: {l}");
        assert!(l.contains("\"repo\""), "no repo field: {l}");
    }

    let _ = std::fs::remove_dir_all(one.parent().unwrap().parent().unwrap());
}
