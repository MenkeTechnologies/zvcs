//! Splitting an index that was never read off disk: the split half must hold
//! *nothing*, because every entry it would stand in for has just been written into
//! the shared half.
//!
//! `prepare_to_write_split_index()` decides, per entry, whether the split half
//! carries a stand-in for it:
//!
//! ```c
//! if (ce->ce_flags & CE_UPDATE_IN_BASE)
//!         ...
//! else if (!ce_uptodate(ce) && is_racy_timestamp(istate, ce))
//!         ...
//! else if (compare_ce_content(...))
//! ```
//!
//! (split-index.c:315-370.) The middle arm is the one this test pins, and it runs
//! through `is_racy_stat()`:
//!
//! ```c
//! return (istate->timestamp.sec &&
//!         ...
//!         istate->timestamp.sec <= sd->sd_mtime.sec
//!         );
//! ```
//!
//! (read-cache.c:355-368.) The leading `istate->timestamp.sec &&` is not a
//! micro-optimisation, it is the whole answer for a from-scratch index:
//! `do_read_index()` zeroes the timestamp and only fills it in from the index
//! file's own `st_mtime` once a read succeeded (read-cache.c:2214-2215 and
//! :2299-2300), so an index that never existed has no timestamp and nothing is
//! racy against it.
//!
//! Dated *now* instead, every file written in the current second looks racily
//! clean — which is exactly the situation `update-index --split-index --add` on
//! files a script has just created — and the split half ends up carrying a
//! name-stripped stand-in for every entry the shared half already holds.
//!
//! Measured on stock git 2.55.0 with ten files created immediately beforehand:
//!
//! ```text
//! $ git update-index --split-index --add f1 … f10
//! $ ls .git | grep shared
//! sharedindex.bf668d4354194569cddcc4a254e758c819c698af
//! $ wc -c .git/index
//! 100
//! ```
//!
//! 100 bytes is a twelve-byte header claiming zero entries, a sixty-byte `link`
//! and a twenty-byte trailer.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    work: PathBuf,
    files: Vec<String>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// Ten files written right now, so their `mtime` is the current second.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-idxsplit-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let mut files = Vec::new();
        for i in 1..=10u32 {
            let name = format!("f{i:02}");
            std::fs::write(work.join(&name), format!("{i}\n")).unwrap();
            files.push(name);
        }
        let f = Fixture { root, work, files };
        f.ok(&["init", "-q", "-b", "main", "."]);
        assert!(!f.work.join(".git/index").exists(), "fixture must start index-less");
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
            .output()
            .unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn index(&self) -> Vec<u8> {
        std::fs::read(self.work.join(".git/index")).expect("an index was written")
    }

    /// The single `sharedindex.<id>` the split left behind.
    fn shared(&self) -> Vec<u8> {
        let mut found: Vec<PathBuf> = std::fs::read_dir(self.work.join(".git"))
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("sharedindex."))
            })
            .collect();
        assert_eq!(found.len(), 1, "exactly one shared index, got {found:?}");
        std::fs::read(found.pop().unwrap()).unwrap()
    }
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes(b.try_into().unwrap())
}

fn entry_count(index: &[u8]) -> u32 {
    assert_eq!(&index[..4], b"DIRC");
    be32(&index[8..12])
}

/// `(signature, body)` for every extension in the file, for an index with no entries.
fn extensions_of_empty_index(index: &[u8]) -> Vec<(String, Vec<u8>)> {
    assert_eq!(entry_count(index), 0, "this helper skips no entries");
    let mut at = 12usize;
    let mut out = Vec::new();
    while at + 8 <= index.len() - 20 {
        let sig = String::from_utf8_lossy(&index[at..at + 4]).into_owned();
        let size = be32(&index[at + 4..at + 8]) as usize;
        out.push((sig, index[at + 8..at + 8 + size].to_vec()));
        at += 8 + size;
    }
    assert_eq!(at, index.len() - 20, "the extension chain must end at the trailer");
    out
}

#[test]
fn a_fresh_split_index_keeps_every_entry_in_the_shared_half() {
    let f = Fixture::new("fresh");
    let mut args: Vec<&str> = vec!["update-index", "--split-index", "--add"];
    args.extend(f.files.iter().map(String::as_str));
    f.ok(&args);

    let index = f.index();
    assert_eq!(
        entry_count(&index),
        0,
        "the split half must stand in for nothing: every entry was just written into \
         the shared half, so none of them is a replacement of it"
    );
    assert_eq!(
        index.len(),
        100,
        "12 header + 8 extension header + 60 `link` body + 20 trailer"
    );

    let exts = extensions_of_empty_index(&index);
    assert_eq!(
        exts.iter().map(|(s, _)| s.as_str()).collect::<Vec<_>>(),
        vec!["link"],
        "the split half carries the link and nothing else"
    );
    let (_, link) = &exts[0];
    assert_eq!(
        link.len(),
        60,
        "20 base id plus a serialised empty ewah for each of the delete and replace bitmaps, \
         20 bytes apiece"
    );
    assert!(
        link[20..].iter().all(|b| *b == 0 || *b == 1),
        "both bitmaps are empty: measured as 00000000 00000001 00000000 00000000 00000000, twice"
    );

    // The base the `link` names must be the file that is actually on disk, and it
    // must hold all ten entries.
    let shared = f.shared();
    assert_eq!(entry_count(&shared), 10, "the shared half holds every entry");
    assert_eq!(
        &link[..20],
        &shared[shared.len() - 20..],
        "`link` names the shared index by its own trailing checksum"
    );
}

/// The same split with every `mtime` an hour in the future, which is the sharp
/// form of the question.
///
/// A from-scratch index has no timestamp, so `is_racy_stat()` returns on its very
/// first term and no `mtime` — however far ahead — can make an entry racy. An
/// index dated *now* instead would find all ten of them racy by a wide margin and
/// carry a stand-in for each, so this pins the zero rather than the accident of
/// how many seconds a test takes.
#[test]
fn a_future_mtime_cannot_make_a_from_scratch_index_racy() {
    let f = Fixture::new("future");
    let ahead = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
    for name in &f.files {
        std::fs::File::options()
            .write(true)
            .open(f.work.join(name))
            .unwrap()
            .set_modified(ahead)
            .unwrap();
    }
    let mut args: Vec<&str> = vec!["update-index", "--split-index", "--add"];
    args.extend(f.files.iter().map(String::as_str));
    f.ok(&args);

    assert_eq!(
        entry_count(&f.index()),
        0,
        "an index with no timestamp of its own has nothing to be racy against"
    );
    assert_eq!(entry_count(&f.shared()), 10);
}

/// The past-dated control: correct either way, so a failure here means something
/// other than the racy-clean test moved.
#[test]
fn the_same_split_is_unchanged_for_files_that_are_not_racy() {
    let f = Fixture::new("old");
    let past = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
    for name in &f.files {
        std::fs::File::options()
            .write(true)
            .open(f.work.join(name))
            .unwrap()
            .set_modified(past)
            .unwrap();
    }
    let mut args: Vec<&str> = vec!["update-index", "--split-index", "--add"];
    args.extend(f.files.iter().map(String::as_str));
    f.ok(&args);

    assert_eq!(entry_count(&f.index()), 0);
    assert_eq!(entry_count(&f.shared()), 10);
}
