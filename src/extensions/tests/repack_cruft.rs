//! `git repack --cruft`: the second pack, the `.mtimes` sidecar that dates it,
//! and what `--cruft-expiration` leaves out of it.
//!
//! `write_cruft_pack()` (repack-cruft.c:40-98) hands `pack-objects --cruft` every
//! local pack — the ones this run wrote as INCLUDE, the rest with a `-` — and
//! `read_cruft_objects()` (pack-objects.c:4300-4357) turns that into "everything
//! the store holds that the new packs do not". With a `--cruft-expiration` it
//! takes the recent objects instead (`obj_is_recent()`, reachable.c:183-192) and
//! traverses out from them, so an expired object goes into no pack at all and is
//! gone once `-d` deletes the pack it was living in.

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

fn counted(dir: &Path, key: &str) -> usize {
    let out = stdout_of(&ok(dir, &["count-objects", "-v"]));
    out.lines()
        .find_map(|l| l.strip_prefix(&format!("{key}: ")))
        .unwrap_or_else(|| panic!("no '{key}' in {out}"))
        .trim()
        .parse()
        .expect("a number")
}

/// The pack stems, each with whether a `.mtimes` sidecar marks it as cruft.
fn packs(dir: &Path) -> Vec<(String, bool)> {
    let pack_dir = dir.join(".git").join("objects").join("pack");
    let mut out: Vec<(String, bool)> = std::fs::read_dir(&pack_dir)
        .expect("read objects/pack")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("idx"))
        .map(|p| {
            let cruft = p.with_extension("mtimes").exists();
            (p.file_stem().unwrap().to_string_lossy().into_owned(), cruft)
        })
        .collect();
    out.sort();
    out
}

/// Two commits, the second thrown away: its commit, tree and blob are
/// unreachable, and with the reflog expired nothing else names them.
fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-cruft-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir fixture");
    ok(&dir, &["init", "-q", "-b", "main"]);
    for name in ["a", "b"] {
        std::fs::write(dir.join(format!("{name}.txt")), format!("{name}\n")).expect("write");
        ok(&dir, &["add", &format!("{name}.txt")]);
        ok(&dir, &["commit", "-qm", name]);
    }
    ok(&dir, &["reset", "-q", "--hard", "HEAD~1"]);
    ok(&dir, &["reflog", "expire", "--expire=all", "--all"]);
    assert_eq!(counted(&dir, "count"), 6, "three reachable objects and three unreachable");
    dir
}

#[test]
fn the_unreachable_objects_go_to_a_pack_of_their_own_with_an_mtimes_sidecar() {
    let dir = fixture("plain");

    ok(&dir, &["repack", "--cruft", "-a", "-d", "-q"]);

    let after = packs(&dir);
    assert_eq!(after.len(), 2, "the reachable pack and the cruft pack: {after:?}");
    assert_eq!(after.iter().filter(|(_, cruft)| *cruft).count(), 1, "{after:?}");
    // Every object is now packed, and none was written twice.
    assert_eq!(counted(&dir, "in-pack"), 6);
    assert_eq!(counted(&dir, "count"), 0, "-d's prune-packed took the loose copies");

    // The unreachable objects are still readable, which is the whole point of a
    // cruft pack over pruning them.
    assert!(ok(&dir, &["fsck", "--no-progress"]).status.success());
}

/// With everything older than the cut there is nothing to pack, `--non-empty`
/// suppresses the pack, and the objects stay where they were.
#[test]
fn an_expiration_that_covers_everything_writes_no_cruft_pack() {
    let dir = fixture("expired");

    ok(&dir, &["repack", "--cruft", "--cruft-expiration=now", "-a", "-d", "-q"]);

    let after = packs(&dir);
    assert_eq!(after.len(), 1, "only the reachable pack: {after:?}");
    assert!(!after[0].1, "and it is not a cruft pack: {after:?}");
    assert_eq!(counted(&dir, "in-pack"), 3);
    // The three unreachable objects were loose to begin with; nothing packed
    // them, so nothing removed them either.
    assert_eq!(counted(&dir, "count"), 3);
}

/// A cut older than the objects makes all of them recent, so the run is the
/// undated one again.
#[test]
fn an_expiration_older_than_the_objects_keeps_all_of_them() {
    let dir = fixture("recent");

    ok(&dir, &["repack", "--cruft", "--cruft-expiration=1.year.ago", "-a", "-d", "-q"]);

    let after = packs(&dir);
    assert_eq!(after.len(), 2, "{after:?}");
    assert_eq!(after.iter().filter(|(_, cruft)| *cruft).count(), 1, "{after:?}");
    assert_eq!(counted(&dir, "in-pack"), 6);
    assert_eq!(counted(&dir, "count"), 0);
}
