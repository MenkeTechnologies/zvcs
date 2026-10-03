//! A path `unpack_trees()` refills from a tree no longer stands on the shared index.
//!
//! `merged_entry()` adds `dup_cache_entry(ce, &o->internal.result)` — a copy of the *tree's*
//! entry, whose `ce->index` is 0 — and keeps the old entry's position only when the two are
//! the same (`copy_cache_entry(merge, old)`, unpack-trees.c:2608-2609).
//! `prepare_to_write_split_index()` writes an entry with `!ce->index` whole into the split
//! half and sets the delete bit of the base entry nothing matches any more
//! (split-index.c:255-272, :361-364, :376-383), and `too_many_not_shared_entries()` counts
//! it (read-cache.c:3313-3318).
//!
//! Matched to the base by path alone, the changed entry was written as a name-stripped
//! stand-in for the base entry instead: never counted as unshared, so the default 20%
//! threshold that makes stock write a fresh shared index was never crossed, and under
//! `splitIndex.maxPercentChange=100` the split half replaced where stock deletes and appends.
//!
//! Measured on stock git 2.56.0, four files with one changed on `side`, a fresh split, then
//! `read-tree -m HEAD side`:
//!
//! ```text
//! maxPercentChange=100: n=1 names f3, delete bitmap {2}, replace bitmap empty, 1 shared index
//! default:              n=0, both bitmaps empty, 2 shared indexes
//! ```
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, UNIX_EPOCH};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// 2001-09-09T01:46:40Z: older than any index this test writes, so nothing is racy.
const PAST: u64 = 1_000_000_000;

struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// `main` with `f1`..`f4`, `side` with `f3` changed, `main` checked out, and a fresh
    /// split index whose shared half holds all four entries.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-splitfresh-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.ok(&["init", "-q", "-b", "main", "."]);
        for i in 1..=4u32 {
            f.write(&format!("f{i}"), &format!("{i}\n"));
        }
        f.ok(&["add", "."]);
        f.ok(&["commit", "-q", "-m", "a"]);
        f.ok(&["checkout", "-q", "-b", "side"]);
        f.write("f3", "side3\n");
        f.ok(&["commit", "-q", "-a", "-m", "s"]);
        f.ok(&["checkout", "-q", "main"]);
        f.date("f3");
        f.ok(&["update-index", "-q", "--really-refresh"]);
        f.ok(&["update-index", "--split-index"]);
        assert_eq!(entry_count(&f.index()), 0, "a fresh split leaves the split half empty");
        f
    }

    fn ok(&self, args: &[&str]) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env_remove("GIT_INDEX_VERSION")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@example.com")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@example.com")
            .output()
            .unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn write(&self, rel: &str, content: &str) {
        std::fs::write(self.work.join(rel), content).unwrap();
        self.date(rel);
    }

    fn date(&self, rel: &str) {
        std::fs::File::options()
            .write(true)
            .open(self.work.join(rel))
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_secs(PAST))
            .unwrap();
    }

    fn index(&self) -> Vec<u8> {
        std::fs::read(self.work.join(".git/index")).unwrap()
    }

    fn shared_indexes(&self) -> usize {
        std::fs::read_dir(self.work.join(".git"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with("sharedindex."))
            .count()
    }
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes(b.try_into().unwrap())
}

fn entry_count(index: &[u8]) -> u32 {
    assert_eq!(&index[..4], b"DIRC");
    be32(&index[8..12])
}

/// The name of the first v2 entry: flags at 60, name from 62.
fn first_name(index: &[u8]) -> &[u8] {
    let len = (u16::from_be_bytes([index[12 + 60], index[12 + 61]]) & 0x0fff) as usize;
    &index[12 + 62..12 + 62 + len]
}

#[test]
fn a_path_the_merge_refilled_is_appended_whole_and_its_base_entry_deleted() {
    let f = Fixture::new("whole");
    f.ok(&["-c", "splitIndex.maxPercentChange=100", "read-tree", "-m", "HEAD", "side"]);

    let index = f.index();
    assert_eq!(entry_count(&index), 1);
    assert_eq!(
        first_name(&index),
        b"f3",
        "the refilled entry is written with its name, not as a name-stripped stand-in"
    );
    // One entry of 62 + 2 name bytes, padded to 72; then `link` and its 68-byte body.
    let link = &index[12 + 72 + 8..12 + 72 + 8 + 68];
    assert_eq!(&index[12 + 72..12 + 76], b"link");
    assert_eq!(
        &link[20..],
        &[
            // delete: 3 bits, two words — the run word, then literal 0b100 (base entry f3)
            0, 0, 0, 3, 0, 0, 0, 2, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 0, 0, 0, 0,
            // replace: empty
            0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ][..],
        "f3's base entry is deleted, and nothing is replaced"
    );
    assert_eq!(f.shared_indexes(), 1, "100% never writes a new shared index");
}

#[test]
fn one_refilled_path_in_four_crosses_the_default_threshold() {
    let f = Fixture::new("threshold");
    f.ok(&["read-tree", "-m", "HEAD", "side"]);

    assert_eq!(
        f.shared_indexes(),
        2,
        "one unshared entry in four is over the default 20%, so a new shared index is written"
    );
    assert_eq!(entry_count(&f.index()), 0, "and it holds every entry");
}

/// The same refill through a branch switch: `switch_branches()` is `twoway_merge()`, and
/// for `f3` it takes `merged_entry()`'s fresh entry.
#[test]
fn a_branch_switch_refills_the_changed_path_from_the_tree() {
    let f = Fixture::new("switch");
    f.ok(&["checkout", "-q", "side"]);
    assert_eq!(f.shared_indexes(), 2);
    assert_eq!(entry_count(&f.index()), 0);
}

/// And through `reset --merge`, `reset_index()`'s `oneway_merge()` with `o->update`.
#[test]
fn a_merge_reset_refills_the_changed_path_from_the_tree() {
    let f = Fixture::new("merge");
    f.ok(&["reset", "-q", "--merge", "side"]);
    assert_eq!(f.shared_indexes(), 2);
    assert_eq!(entry_count(&f.index()), 0);
}

/// `reset --keep` runs `reset_index()` twice — `twoway_merge()`, then a `MIXED`
/// `oneway_merge()` — over one in-memory index and writes it once (builtin/reset.c:522-530).
/// Written in between and read back, the second pass no longer knew the first had just
/// written `f3`, found it racily clean against the intermediate index, and kept a stand-in.
#[test]
fn a_keep_reset_writes_its_two_passes_once() {
    let f = Fixture::new("keep");
    f.ok(&["reset", "-q", "--keep", "side"]);
    assert_eq!(f.shared_indexes(), 2);
    assert_eq!(entry_count(&f.index()), 0);
}

/// A fast-forward is `checkout_fast_forward()`, an `unpack_trees()` whose result inherits
/// the source index's shared half (unpack-trees.c:1944-1959). Built from the tree alone, the
/// result had none, and the merge wrote the repository's split index back whole.
#[test]
fn a_fast_forward_merge_keeps_the_index_split() {
    let f = Fixture::new("ff");
    f.ok(&["merge", "-q", "side"]);
    assert_eq!(&f.index()[12..16], b"link", "the index is still split");
    assert_eq!(f.shared_indexes(), 2, "and f3, refilled, crossed the 20% default");
    assert_eq!(entry_count(&f.index()), 0);
}

/// A clean pick checks its result out through `unpack_trees()` as well, so it keeps the
/// shared half too; rebuilt from the tree alone it wrote the split index back whole.
#[test]
fn a_clean_cherry_pick_keeps_the_index_split() {
    let f = Fixture::new("pick");
    f.ok(&["cherry-pick", "side"]);
    assert_eq!(&f.index()[12..16], b"link", "the index is still split");
    assert_eq!(f.shared_indexes(), 2);
    assert_eq!(entry_count(&f.index()), 0);
}
