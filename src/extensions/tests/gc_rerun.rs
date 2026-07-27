//! `gc` must reinstall pack artifacts a previous run left read-only.
//!
//! git installs every pack artifact `0444` (verified against git 2.55.0, whose
//! `gc` leaves `-r--r--r--` on `.pack`, `.idx`, `.rev` and `.mtimes`), and a pack
//! whose object set has not changed hashes to the name it already has on disk.
//! Writing an `.idx`/`.rev`/`.mtimes` straight to that path therefore fails with
//! `EACCES` — reported as `write …/pack-<hash>.idx: Permission denied` — while
//! renaming into place, which is how git installs them, replaces the destination
//! whatever its mode is.
//!
//! The sequence this came from: `git gc` writes a cruft pack, then `zvcs gc`
//! reproduces that pack byte-for-byte (few objects, no deltas) and so collides
//! with the read-only name. The fixture reproduces the state directly rather than
//! shelling out to another git, by sealing the first run's own artifacts.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// The binary under test with the ambient user and system config kept out, so a
/// global `core.autocrlf` or `gc.*` cannot change what the run does.
fn cmd(dir: &Path, args: &[&str]) -> Command {
    let home = dir.join(".isolated-home");
    std::fs::create_dir_all(&home).unwrap();
    let mut c = Command::new(BIN);
    c.args(args)
        .current_dir(dir)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1");
    c
}

fn git(dir: &Path, args: &[&str]) {
    let out = cmd(dir, args).output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repository with a couple of commits and one unreachable object, so `gc`
/// writes a cruft pack and its `.mtimes` beside the ordinary pack and index.
fn fixture() -> PathBuf {
    let repo = std::env::temp_dir().join(format!("zvcs-gcrerun-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&repo);
    std::fs::create_dir_all(&repo).unwrap();
    let repo = repo.canonicalize().unwrap();
    git(&repo, &["init", "-q", "-b", "main", "."]);
    git(&repo, &["config", "user.email", "alice@example.com"]);
    git(&repo, &["config", "user.name", "Alice"]);
    for i in 0..2 {
        std::fs::write(repo.join("f"), format!("v{i}\n")).unwrap();
        git(&repo, &["add", "f"]);
        git(&repo, &["commit", "-q", "-m", &format!("c{i}")]);
    }
    // Unreachable once the branch never records it: a dangling blob for the
    // cruft-pack side of the run.
    std::fs::write(repo.join("loose"), "unreferenced\n").unwrap();
    git(&repo, &["hash-object", "-w", "loose"]);
    std::fs::remove_file(repo.join("loose")).unwrap();
    repo
}

fn gc(repo: &Path) -> Output {
    cmd(repo, &["gc", "-q"]).output().unwrap()
}

fn artifacts(repo: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(repo.join(".git/objects/pack"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Set every pack artifact read-only, the mode git installs them with.
fn seal_pack_dir(repo: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let dir = repo.join(".git/objects/pack");
    for entry in std::fs::read_dir(&dir).unwrap().filter_map(Result::ok) {
        std::fs::set_permissions(&entry.path(), std::fs::Permissions::from_mode(0o444)).unwrap();
    }
    // A privileged user writes through the mode, which would make the assertion
    // below pass for the wrong reason.
    let probe = dir.join("probe");
    std::fs::write(&probe, "x").unwrap();
    std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o444)).unwrap();
    let blocked = std::fs::write(&probe, "y").is_err();
    let _ = std::fs::remove_file(&probe);
    blocked
}

#[test]
fn a_second_gc_reinstalls_read_only_pack_artifacts() {
    let repo = fixture();

    let first = gc(&repo);
    assert!(
        first.status.success(),
        "first gc failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let after_first = artifacts(&repo);
    assert!(
        after_first.iter().any(|n| n.ends_with(".idx")),
        "first gc wrote no index: {after_first:?}"
    );
    if !seal_pack_dir(&repo) {
        eprintln!("skipped: writes through a read-only mode succeed here");
        return;
    }

    let second = gc(&repo);
    assert!(
        second.status.success(),
        "second gc failed over the artifacts the first one left: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(
        after_first,
        artifacts(&repo),
        "the same object set must reinstall the same artifacts"
    );
    assert!(
        !artifacts(&repo).iter().any(|n| n.starts_with("tmp_")),
        "a temporary file survived the run: {:?}",
        artifacts(&repo)
    );
}
