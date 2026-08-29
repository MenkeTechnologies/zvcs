//! The two object sets `repack` must not treat as ordinary reachable objects:
//! the parents of a shallow boundary commit, and the objects a promisor pack
//! holds.
//!
//! Both used to end the same way — the traversal named an object the repository
//! does not have and the pack writer died on it. They are separate rules:
//!
//!   * `.git/shallow` installs a graft with `nr_parent = -1` for each commit it
//!     names (`register_shallow()`, shallow.c:34-47), so the walk stops there;
//!   * a repository with a promisor remote gets `--exclude-promisor-objects`
//!     (builtin/repack.c:354-355), which marks every object a `.promisor` pack
//!     holds UNINTERESTING before the walk (revision.c:4001-4003) — and those
//!     objects are then written to a promisor pack of their own by
//!     `repack_promisor_objects()` (repack-promisor.c:82-111).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .output()
        .expect("run binary")
}

fn ok(dir: &Path, args: &[&str]) -> Output {
    let out = run(dir, args);
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    out
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// One `key: value` line of `count-objects -v`.
fn counted(dir: &Path, key: &str) -> usize {
    let out = stdout_of(&ok(dir, &["count-objects", "-v"]));
    out.lines()
        .find_map(|l| l.strip_prefix(&format!("{key}: ")))
        .unwrap_or_else(|| panic!("no '{key}' in {out}"))
        .trim()
        .parse()
        .expect("a number")
}

/// The pack stems under `objects/pack`, each with whether a `.promisor` file
/// sits beside it.
fn packs(dir: &Path) -> Vec<(String, bool)> {
    let pack_dir = dir.join(".git").join("objects").join("pack");
    let mut out: Vec<(String, bool)> = std::fs::read_dir(&pack_dir)
        .expect("read objects/pack")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("idx"))
        .map(|p| {
            let promisor = p.with_extension("promisor").exists();
            (p.file_stem().unwrap().to_string_lossy().into_owned(), promisor)
        })
        .collect();
    out.sort();
    out
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-repackpr-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir fixture");
    ok(&dir, &["init", "-q", "-b", "main"]);
    dir
}

/// One commit per file, so every commit brings a tree and a blob of its own and
/// the object counts below are exact.
fn commit_files(dir: &Path, names: &[&str]) {
    for name in names {
        std::fs::write(dir.join(format!("{name}.txt")), format!("{name}\n")).expect("write");
        ok(dir, &["add", &format!("{name}.txt")]);
        ok(dir, &["commit", "-qm", name]);
    }
}

/// A commit named by `.git/shallow` contributes no parents, so its history is
/// not reachable and does not go into the pack. The reflog has to be expired
/// first: `--reflog` makes every recorded tip a root of its own, and the ones
/// behind the boundary are still perfectly good roots.
#[test]
fn a_shallow_boundary_commit_ends_the_traversal_that_repack_packs() {
    let dir = scratch("shallow");
    commit_files(&dir, &["a", "b", "c"]);
    assert_eq!(counted(&dir, "count"), 9, "three commits, each with a tree and a blob");

    let boundary = stdout_of(&ok(&dir, &["rev-parse", "HEAD~1"]));
    std::fs::write(dir.join(".git").join("shallow"), &boundary).expect("write .git/shallow");
    ok(&dir, &["reflog", "expire", "--expire=all", "--all"]);

    ok(&dir, &["repack", "-a", "-d"]);
    // Reachable: the two commits from the boundary on, their trees, and the
    // three blobs the newest tree names. Left loose: the first commit and its
    // tree, which only the graft-cut parent link led to.
    assert_eq!(counted(&dir, "in-pack"), 7);
    assert_eq!(counted(&dir, "count"), 2);

    // Without the graft the same walk reaches everything, which is what makes
    // the assertion above about the graft rather than about the reflog.
    std::fs::remove_file(dir.join(".git").join("shallow")).expect("remove .git/shallow");
    ok(&dir, &["repack", "-a", "-d"]);
    assert_eq!(counted(&dir, "in-pack"), 9);
    assert_eq!(counted(&dir, "count"), 0);
}

/// A repository whose only pack is marked `.promisor`, with a promisor remote
/// configured and three loose objects of its own on top.
fn promisor_fixture(tag: &str) -> PathBuf {
    let dir = scratch(tag);
    commit_files(&dir, &["a", "b"]);
    ok(&dir, &["repack", "-a", "-d", "-q"]);
    for (stem, _) in packs(&dir) {
        let path = dir.join(".git").join("objects").join("pack").join(format!("{stem}.promisor"));
        std::fs::write(path, b"").expect("write .promisor");
    }
    // `promisor_remote_init()` finds a remote through either of these.
    ok(&dir, &["config", "remote.origin.url", "./peer.git"]);
    ok(&dir, &["config", "remote.origin.promisor", "true"]);

    commit_files(&dir, &["new"]);
    ok(&dir, &["reflog", "expire", "--expire=all", "--all"]);
    dir
}

#[test]
fn a_promisor_repack_writes_the_promisor_objects_to_a_promisor_pack_of_their_own() {
    let dir = promisor_fixture("promisor");

    ok(&dir, &["repack", "-a", "-d"]);
    let after = packs(&dir);
    assert_eq!(after.len(), 2, "one promisor pack and one ordinary one: {after:?}");
    let promisor: Vec<&(String, bool)> = after.iter().filter(|(_, p)| *p).collect();
    assert_eq!(promisor.len(), 1, "exactly one pack keeps the mark: {after:?}");

    // Six objects came out of the promisor pack and three are the local commit's
    // own; nothing was left loose and nothing was written twice.
    assert_eq!(counted(&dir, "in-pack"), 9);
    assert_eq!(counted(&dir, "count"), 0);
    assert_eq!(counted(&dir, "packs"), 2);

    // A second run has nothing new for the main pack-objects, and stays silent
    // about it: the notice is gated on the run's `names`, which the promisor
    // pack is in.
    let again = ok(&dir, &["repack", "-a", "-d"]);
    assert_eq!(stdout_of(&again), "");
    assert_eq!(packs(&dir).len(), 2);
}

/// `--filter` splits the *main* pack's objects; the promisor pack is already
/// excluded from it by name, so the filtered pack holds nothing twice.
#[test]
fn a_filtered_promisor_repack_does_not_copy_the_promisor_pack_into_the_filtered_one() {
    let dir = promisor_fixture("filter");

    ok(&dir, &["repack", "-a", "-d", "--filter=blob:none"]);
    let after = packs(&dir);
    assert_eq!(after.iter().filter(|(_, p)| *p).count(), 1, "the mark survives: {after:?}");
    // The promisor pack, the filtered blobs, and what is left of the main pack.
    assert_eq!(counted(&dir, "packs"), 3);
    assert_eq!(counted(&dir, "in-pack"), 9);
    assert_eq!(counted(&dir, "count"), 0);
}
