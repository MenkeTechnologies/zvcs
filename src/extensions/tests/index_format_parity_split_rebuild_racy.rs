//! An index rebuilt from a tree is dated by the index it replaces, so an entry that was
//! racily clean in the old one is racily clean in the new one too.
//!
//! ```c
//! o->internal.result.timestamp.sec = o->src_index->timestamp.sec;
//! o->internal.result.timestamp.nsec = o->src_index->timestamp.nsec;
//! ```
//!
//! (unpack-trees.c:1941-1942.) `prepare_to_write_split_index()` then asks
//! `is_racy_timestamp()` of every entry still standing on the shared half, and moves each
//! racy one into the split half so `do_write_index()` can smudge it (split-index.c:283-306).
//!
//! The rebuilt index here started with no timestamp at all, so nothing in it was racy: the
//! split half came out empty where stock's carries a stand-in for every entry, and stock
//! reading it back saw `flags: 8000000` (`CE_UPDATE_IN_BASE`) on each entry of its own
//! index and `flags: 0` on each entry of this one.
//!
//! Measured on stock git 2.56.0 — four files dated 2001, a fresh split, the index file
//! re-dated to the files' own second, then `reset -q --hard`:
//!
//! ```text
//! hdr DIRC v2 n=4
//! names <strip> <strip> <strip> <strip>
//! link 68: delete bitmap empty, replace bitmap 0x0f
//! ```
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// 2001-09-09T01:46:40Z: old enough that no index written by this test can be older.
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
    /// Four committed files dated [`PAST`], then `update-index --split-index` so the
    /// shared half holds all four and the split half none.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-splitracy-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.ok(&["init", "-q", "-b", "main", "."]);
        for i in 1..=4u32 {
            let name = format!("f{i}");
            std::fs::write(f.work.join(&name), format!("{i}\n")).unwrap();
            f.date(&name, PAST);
        }
        f.ok(&["add", "."]);
        f.ok(&["commit", "-q", "-m", "a"]);
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

    fn date(&self, rel: &str, secs: u64) {
        std::fs::File::options()
            .write(true)
            .open(self.work.join(rel))
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_secs(secs))
            .unwrap();
    }

    fn index(&self) -> Vec<u8> {
        std::fs::read(self.work.join(".git/index")).unwrap()
    }
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes(b.try_into().unwrap())
}

fn entry_count(index: &[u8]) -> u32 {
    assert_eq!(&index[..4], b"DIRC");
    be32(&index[8..12])
}

/// The `link` body, found by walking the extension chain behind `n` name-stripped
/// entries — each a 62-byte record padded to 64, the only shape a stand-in has in v2.
fn link_of(index: &[u8]) -> Vec<u8> {
    let mut at = 12 + 64 * entry_count(index) as usize;
    while at + 8 <= index.len() - 20 {
        let size = be32(&index[at + 4..at + 8]) as usize;
        if &index[at..at + 4] == b"link" {
            return index[at + 8..at + 8 + size].to_vec();
        }
        at += 8 + size;
    }
    panic!("no `link` extension");
}

#[test]
fn a_hard_reset_carries_every_racy_entry_into_the_split_half() {
    let f = Fixture::new("racy");
    // The index's own second is now the files' second: every entry is racily clean.
    f.date(".git/index", PAST);
    f.ok(&["reset", "-q", "--hard"]);

    let index = f.index();
    assert_eq!(
        entry_count(&index),
        4,
        "the rebuilt index inherits the old one's timestamp, so all four entries are racy \
         and each gets a stand-in"
    );
    let link = link_of(&index);
    assert_eq!(link.len(), 68, "20 base id + a 20-byte empty delete ewah + a 28-byte replace ewah");
    // The replace ewah: 4 bits, one run word, then the literal word 0b1111.
    assert_eq!(
        &link[40..],
        &[0, 0, 0, 4, 0, 0, 0, 2, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x0f, 0, 0, 0, 0][..],
        "every base entry is replaced"
    );
}

/// The control: the same reset over an index that is newer than every file it records.
#[test]
fn a_hard_reset_over_a_clean_split_index_leaves_the_split_half_empty() {
    let f = Fixture::new("clean");
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    f.date(".git/index", now);
    f.ok(&["reset", "-q", "--hard"]);
    assert_eq!(entry_count(&f.index()), 0);
}
