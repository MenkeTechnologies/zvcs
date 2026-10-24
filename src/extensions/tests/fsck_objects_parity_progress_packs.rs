//! `git fsck-objects --progress` in a repository with packs: the meters
//! `odb_fsck()` draws, byte for byte against stock git 2.56.0.
//!
//!   1. **"Checking object directories" advances once per existing
//!      `.git/objects/??` directory**, to `nr + 1` (`fsck_subdir()`,
//!      odb/source-loose.c:1101-1106), and redraws only when the whole
//!      percentage changes (progress.c:127-138).
//!   2. **"Checking objects" counts packs in pack-list order.** Its total is
//!      every pack's object count and `verify_packfile()` draws once after each
//!      pack (pack-check.c:170-178), so the intermediate line names the first
//!      pack's size. With equal `.pack` mtimes `sort_pack()` ties, and the list
//!      order is the one `snapshot_refs()`' reads left behind: resolving
//!      `HEAD:README.md` reads the commit and its tree, then `parse_object()`
//!      reads the blob (tree-walk.c:605-635, object.c:335-394), and every read
//!      moves the pack that answered it to the front (packfile.c find_pack_entry).
//!
//! Fixtures are built with the binary under test; dates are pinned so object
//! ids, and with them the loose subdirectories, are fixed.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("TERM", "dumb")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run binary under test")
}

fn ok(dir: &Path, home: &Path, args: &[&str]) {
    let out = run(dir, home, args);
    assert!(
        out.status.success(),
        "{args:?} failed ({}): {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Two packs — four commits' worth, then one more commit's — with loose copies
/// of the second pack's objects left behind, and both `.pack` files given the
/// same mtime so the pack list's order comes from the reads alone.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-fsck-progress-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    let home = root.join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    ok(&repo, &home, &["init", "-q", "-b", "main", "."]);
    std::fs::write(repo.join("README.md"), "readme\n").unwrap();
    ok(&repo, &home, &["add", "README.md"]);
    ok(&repo, &home, &["commit", "-qm", "readme"]);
    for n in 1..=3 {
        let name = format!("f{n}");
        std::fs::write(repo.join(&name), format!("{n}\n")).unwrap();
        ok(&repo, &home, &["add", &name]);
        ok(&repo, &home, &["commit", "-qm", &name]);
    }
    ok(&repo, &home, &["-c", "pack.threads=1", "repack", "-adq"]);
    std::fs::write(repo.join("g"), "g\n").unwrap();
    ok(&repo, &home, &["add", "g"]);
    ok(&repo, &home, &["commit", "-qm", "g"]);
    ok(&repo, &home, &["-c", "pack.threads=1", "repack", "-q"]);

    let pack_dir = repo.join(".git/objects/pack");
    let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
    for entry in std::fs::read_dir(&pack_dir).unwrap().flatten() {
        if entry.path().extension().is_some_and(|x| x == "pack") {
            std::fs::File::open(entry.path())
                .unwrap()
                .set_modified(when)
                .unwrap();
        }
    }
    (repo, home)
}

fn progress_stderr(repo: &Path, home: &Path, args: &[&str]) -> String {
    let mut argv = vec!["fsck-objects", "--progress"];
    argv.extend_from_slice(args);
    let out = run(repo, home, &argv);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stderr).into_owned()
}

const REFS: &str = "Checking ref database: 100% (1/1)\rChecking ref database: 100% (1/1), done.\n";
const DIRS: &str = "Checking object directories:   0% (2/256)\rChecking object directories:  87% (223/256)\rChecking object directories:  92% (237/256)\rChecking object directories: 100% (256/256)\rChecking object directories: 100% (256/256), done.\n";
const LARGE_FIRST: &str = "Checking objects:   0% (0/15)\rChecking objects:  80% (12/15)\rChecking objects: 100% (15/15)\rChecking objects: 100% (15/15), done.\n";
const SMALL_FIRST: &str = "Checking objects:   0% (0/15)\rChecking objects:  20% (3/15)\rChecking objects: 100% (15/15)\rChecking objects: 100% (15/15), done.\n";

#[test]
fn object_directories_meter_steps_through_existing_subdirectories() {
    let (repo, home) = fixture("dirs");
    let err = progress_stderr(&repo, &home, &["--no-full", "main"]);
    assert_eq!(err, format!("{REFS}{DIRS}"));
}

#[test]
fn checking_objects_meter_follows_the_pack_list_the_snapshot_reads_left() {
    let (repo, home) = fixture("order");
    // The commit and its tree are in the small pack, README.md's blob in the
    // large one: the blob read is last, so the large pack leads.
    let err = progress_stderr(&repo, &home, &["HEAD:README.md"]);
    assert_eq!(err, format!("{REFS}{DIRS}{LARGE_FIRST}"));
    // `main`'s `parse_object()` reads the commit after the blob, so the small
    // pack leads instead.
    let err = progress_stderr(&repo, &home, &["HEAD:README.md", "main"]);
    assert_eq!(err, format!("{REFS}{DIRS}{SMALL_FIRST}"));
}
