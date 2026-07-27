//! A multi-pack-index that outlives the packs it names must not hide objects.
//!
//! `multi-pack-index` maps an object id to `(pack, offset)`. When `repack -d` or
//! `gc` deletes a pack the index still names, every object the index attributes
//! to that pack resolves to a file that is gone. git survives this: it falls back
//! to the packs that are still there, so `log --stat`, `show` and every other
//! diff keep working. Two behaviors keep that true here, and both are asserted
//! below:
//!
//! * the object store finishes scanning the remaining indices when the pack an
//!   index points at cannot be opened, rather than reporting the object missing;
//! * `repack -d` and `gc` delete a multi-pack-index that names a pack they just
//!   removed, so the stale state is not created in the first place.
//!
//! The regression this pins: after a `gc`, `log --stat` failed with "An object
//! with id <tree> could not be found" for a tree that `cat-file` in the same
//! repository printed without complaint.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Run the binary under test in `dir`, asserting success.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new(BIN).args(args).current_dir(dir).output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A repository whose objects sit in two packs, plus a multi-pack-index naming
/// both. Each commit is packed on its own so the index has more than one pack to
/// attribute objects to, which is what makes a later `-a` repack invalidate it.
fn fixture(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-midx-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let repo = root.canonicalize().unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "alice@example.com"]);
    git(&repo, &["config", "user.name", "Alice"]);
    for i in 0..2 {
        let dir = repo.join(format!("d{i}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f"), format!("line {i}\n")).unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", &format!("c{i}")]);
        git(&repo, &["repack", "-q", "-d"]);
    }
    git(&repo, &["multi-pack-index", "write"]);
    assert!(midx(&repo).is_file(), "fixture must have a multi-pack-index");
    assert!(pack_count(&repo) >= 2, "fixture must have more than one pack");
    repo
}

fn midx(repo: &Path) -> PathBuf {
    repo.join(".git/objects/pack/multi-pack-index")
}

fn pack_count(repo: &Path) -> usize {
    std::fs::read_dir(repo.join(".git/objects/pack"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(".pack"))
        .count()
}

/// `repack -ad` rewrites every object into one new pack and deletes the old ones,
/// which leaves the multi-pack-index describing files that no longer exist. git
/// removes the index as part of that deletion.
#[test]
fn repack_deletes_a_multi_pack_index_it_invalidates() {
    let repo = fixture("repack");
    git(&repo, &["repack", "-q", "-adf"]);
    assert_eq!(pack_count(&repo), 1, "-a repacks everything into one pack");
    assert!(
        !midx(&repo).exists(),
        "the multi-pack-index named the packs -d just deleted and must go with them"
    );
}

/// `gc` packs and prunes through the same path, so it has to drop the index too.
#[test]
fn gc_deletes_a_multi_pack_index_it_invalidates() {
    let repo = fixture("gc");
    git(&repo, &["gc", "-q"]);
    assert!(
        !midx(&repo).exists(),
        "gc rewrote the packs the multi-pack-index named and must delete it"
    );
}

/// The store's own resilience, independent of who wrote the stale index: with a
/// multi-pack-index restored on top of a repository whose packs have all been
/// replaced, diffs still resolve every object from the surviving pack.
#[test]
fn diffs_survive_a_multi_pack_index_left_behind() {
    let repo = fixture("stale");
    let saved = std::fs::read(midx(&repo)).unwrap();
    git(&repo, &["repack", "-q", "-adf"]);
    // Recreate exactly what a version that forgot the index would have left: the
    // packs it names are gone, their objects now live in the pack just written.
    std::fs::write(midx(&repo), &saved).unwrap();

    let out = git(&repo, &["log", "--oneline", "--stat"]);
    assert!(
        out.contains("d0/f") && out.contains("d1/f"),
        "every commit's diff must resolve through the surviving pack, got:\n{out}"
    );
    let show = git(&repo, &["show", "--stat", "--oneline", "HEAD"]);
    assert!(show.contains("d1/f"), "show must resolve the tree too, got:\n{show}");
}
