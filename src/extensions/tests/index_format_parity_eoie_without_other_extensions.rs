//! `EOIE` is written for `record_eoie()` alone, not for "some other extension was
//! written".
//!
//! ```c
//! offset = hashfile_total(f);
//!
//! /*
//!  * The extension headers must be hashed on their own for the
//!  * EOIE extension. Create a hashfile here to compute that hash.
//!  */
//! if (offset && record_eoie()) {
//!         CALLOC_ARRAY(eoie_c, 1);
//!         the_hash_algo->init_fn(eoie_c);
//! }
//! ```
//!
//! (read-cache.c:2951-2960; the write itself is at :3060-3070 and is gated on
//! `eoie_c` alone.) `offset` is the file position one past the last entry, so it is
//! at least the twelve bytes of the header and can never be zero — the condition
//! reduces to `record_eoie()`. An index with no cache-tree, no resolve-undo and no
//! `link` still gets an `EOIE`, and the hash it carries is then the hash of nothing
//! at all, because that hash covers only the extension headers that preceded it
//! (`write_index_ext_header()`, read-cache.c:2543-2558).
//!
//! Measured on stock git 2.55.0:
//!
//! ```text
//! $ git -c index.recordEndOfIndexEntries=true update-index --add a
//! $ xxd .git/index | tail -3
//! ...  EOIE, size 24, offset 76, da39a3ee5e6b4b0d3255bfef95601890afd80709
//! ```
//!
//! `da39a3ee…` is SHA-1 of the empty string.
//!
//! `record_eoie()` also has a second spelling: with `index.recordEndOfIndexEntries`
//! unset it follows `index.threads`, "as a convenience … written by default if the
//! user explicitly requested threaded index reads" (read-cache.c:2766-2771).
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// SHA-1 of the empty byte string — the `EOIE` hash of an index whose `EOIE` is
/// the only extension in the file.
const SHA1_OF_NOTHING: &str = "da39a3ee5e6b4b0d3255bfef95601890afd80709";

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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-idxeoie-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.ok(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), b"a\n").unwrap();
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
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes(b.try_into().unwrap())
}

/// Walk the extension chain, returning `(signature, body)` for each one.
///
/// Entries are skipped by walking them: a version 2 index pads every entry to an
/// eight-byte boundary measured from the start of the entry (`ce_write_entry()`,
/// read-cache.c:2601).
fn extensions(index: &[u8]) -> Vec<(String, Vec<u8>)> {
    assert_eq!(&index[..4], b"DIRC");
    assert_eq!(be32(&index[4..8]), 2, "this fixture writes version 2");
    let count = be32(&index[8..12]) as usize;

    let mut at = 12usize;
    for _ in 0..count {
        let start = at;
        at += 40 + 20; // stat data and the SHA-1
        let flags = u16::from_be_bytes(index[at..at + 2].try_into().unwrap());
        at += 2;
        assert_eq!(flags & 0x4000, 0, "a version 2 entry has no extended flag word");
        at += (flags & 0xfff) as usize + 1; // the path and its NUL
        at = start + (at - start).next_multiple_of(8);
    }

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

/// `(offset, hash)` of the `EOIE` extension, which must be the last one in the file.
fn eoie(index: &[u8]) -> (u32, String) {
    let exts = extensions(index);
    let (sig, body) = exts.last().expect("at least one extension");
    assert_eq!(sig, "EOIE", "EOIE must be the last extension, got {exts:?}", );
    assert_eq!(body.len(), 24, "a four-byte offset and a SHA-1");
    (
        be32(&body[..4]),
        body[4..].iter().map(|b| format!("{b:02x}")).collect(),
    )
}

#[test]
fn eoie_is_written_even_when_it_is_the_only_extension() {
    let f = Fixture::new("alone");
    f.ok(&["-c", "index.recordEndOfIndexEntries=true", "update-index", "--add", "a"]);

    let index = f.index();
    let exts = extensions(&index);
    assert_eq!(
        exts.iter().map(|(s, _)| s.as_str()).collect::<Vec<_>>(),
        vec!["EOIE"],
        "`update-index --add` builds no cache-tree, so EOIE stands alone"
    );

    let (offset, hash) = eoie(&index);
    assert_eq!(
        offset, 76,
        "one past the single entry: 12 header + 62 payload padded to 64"
    );
    assert_eq!(
        hash, SHA1_OF_NOTHING,
        "the EOIE hash covers the extension headers before it, and there are none"
    );
}

/// The same index without the key: no `EOIE`, so the extension is not simply
/// unconditional.
#[test]
fn eoie_is_absent_when_nothing_asked_for_it() {
    let f = Fixture::new("off");
    f.ok(&["update-index", "--add", "a"]);
    assert!(
        extensions(&f.index()).is_empty(),
        "an unconfigured `update-index --add` writes no extension at all"
    );
}

/// `index.threads` is `record_eoie()`'s fallback, and "enabled" means a value that
/// is not one — so `index.threads=2` writes an `EOIE` and `index.threads=1` does not.
#[test]
fn index_threads_is_the_fallback_for_record_eoie() {
    let f = Fixture::new("threads2");
    f.ok(&["-c", "index.threads=2", "update-index", "--add", "a"]);
    let (offset, hash) = eoie(&f.index());
    assert_eq!(offset, 76);
    assert_eq!(hash, SHA1_OF_NOTHING);

    let g = Fixture::new("threads1");
    g.ok(&["-c", "index.threads=1", "update-index", "--add", "a"]);
    assert!(
        extensions(&g.index()).is_empty(),
        "one thread is not a request for threaded reads"
    );
}

/// With another extension present the hash is no longer the empty one, and the
/// offset still points one past the entries rather than at `EOIE` itself.
#[test]
fn eoie_records_where_the_extensions_begin_not_where_it_begins() {
    let f = Fixture::new("withtree");
    f.ok(&["-c", "index.recordEndOfIndexEntries=true", "add", "a"]);
    f.ok(&["-c", "index.recordEndOfIndexEntries=true", "write-tree"]);

    let index = f.index();
    let exts = extensions(&index);
    let names: Vec<&str> = exts.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(names, vec!["TREE", "EOIE"], "EOIE is written last");

    let (offset, hash) = eoie(&index);
    assert_eq!(offset, 76, "still one past the entries");
    assert_ne!(
        hash, SHA1_OF_NOTHING,
        "the TREE header is now part of what the EOIE hash covers"
    );
}
