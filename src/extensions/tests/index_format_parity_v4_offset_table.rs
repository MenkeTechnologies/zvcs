//! Index version 4 and the `IEOT` extension together: what the *first* entry of
//! each offset-table block encodes its path against.
//!
//! Version 4 stores a path as `varint(strip) + suffix + NUL`, where `strip` is how
//! many bytes to drop from the end of the previous entry's path
//! (`ce_write_entry()`, read-cache.c:2601). `IEOT` exists so a reader can start a
//! thread at an arbitrary block boundary (`load_cache_entries_threaded()`,
//! read-cache.c:2126), and such a thread begins with an *empty* previous name — so
//! the first entry of every block has to be self-contained. git arranges that in
//! the entry loop:
//!
//! ```c
//! if (ieot && i && (i % ieot_entries == 0)) {
//!         ieot->entries[ieot->nr].nr = nr;
//!         ieot->entries[ieot->nr].offset = offset;
//!         ieot->nr++;
//!         /*
//!          * If we have a V4 index, set the first byte to an invalid
//!          * character to ensure there is nothing common with the previous
//!          * entry
//!          */
//!         if (previous_name)
//!                 previous_name->buf[0] = 0;
//!         nr = 0;
//!
//!         offset = hashfile_total(f);
//! }
//! ```
//!
//! (read-cache.c:2917-2931.) Note *which* byte is poisoned and what is left alone:
//! the length is untouched, so `ce_write_entry()` finds `common == 0` — no path
//! byte is NUL — and still computes `to_remove = previous_name->len`. The first
//! entry of a block therefore carries a strip count equal to the whole previous
//! path plus its own path in full, which is not the same as a strip count of zero.
//!
//! Measured on stock git 2.55.0 over 40 paths at `index.threads=4`: the blocks are
//! `[(12, 10), (725, 10), (1438, 10), (2151, 10)]` and entry 10, `dir01/file011.txt`,
//! follows `dir01/file006.txt` with `strip=17` and the whole name as its suffix,
//! where an unpoisoned buffer would have produced `strip=6, suffix="11.txt"`.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// One decoded on-disk entry: where it starts, what its path bytes decode to, and
/// the version 4 encoding it was stored with.
struct Entry {
    offset: usize,
    path: Vec<u8>,
    strip: usize,
    suffix: Vec<u8>,
}

struct Fixture {
    root: PathBuf,
    work: PathBuf,
    paths: Vec<String>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// 40 files spread over 5 directories, so that consecutive paths share long
    /// prefixes and the compression has something to say.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-idxieot-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let mut paths = Vec::new();
        for i in 0..40u32 {
            let dir = format!("dir{:02}", i % 5);
            std::fs::create_dir_all(work.join(&dir)).unwrap();
            let path = format!("{dir}/file{i:03}.txt");
            std::fs::write(work.join(&path), format!("x{i}\n")).unwrap();
            paths.push(path);
        }
        paths.sort();
        let f = Fixture {
            root,
            work,
            paths,
        };
        f.ok(&["init", "-q", "-b", "main", "."]);
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

/// `decode_varint()` (varint.c:3): most-significant group first, every
/// continuation group biased by one.
fn decode_varint(data: &[u8], at: &mut usize) -> usize {
    let mut byte = data[*at];
    *at += 1;
    let mut value = (byte & 0x7f) as usize;
    while byte & 0x80 != 0 {
        value += 1;
        byte = data[*at];
        *at += 1;
        value = (value << 7) + (byte & 0x7f) as usize;
    }
    value
}

/// Decode the entries of a version 4 index, returning both the reconstructed path
/// and the encoding each entry was stored with.
fn v4_entries(index: &[u8]) -> (Vec<Entry>, usize) {
    assert_eq!(&index[..4], b"DIRC");
    assert_eq!(be32(&index[4..8]), 4, "the index must be version 4");
    let count = be32(&index[8..12]) as usize;

    let mut at = 12usize;
    let mut previous: Vec<u8> = Vec::new();
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let offset = at;
        at += 40; // stat data
        at += 20; // object id (SHA-1)
        let flags = u16::from_be_bytes(index[at..at + 2].try_into().unwrap());
        at += 2;
        if flags & 0x4000 != 0 {
            at += 2; // the version 3 extended flag word
        }
        let strip = decode_varint(index, &mut at);
        let nul = index[at..].iter().position(|b| *b == 0).expect("a NUL-terminated suffix") + at;
        let suffix = index[at..nul].to_vec();
        at = nul + 1;

        let mut path = previous[..previous.len() - strip].to_vec();
        path.extend_from_slice(&suffix);
        previous = path.clone();
        out.push(Entry {
            offset,
            path,
            strip,
            suffix,
        });
    }
    (out, at)
}

/// The `(offset, entry count)` pairs of the `IEOT` extension, found by walking the
/// extension chain from `one_past_entries`.
fn ieot_blocks(index: &[u8], one_past_entries: usize) -> Vec<(u32, u32)> {
    let mut at = one_past_entries;
    while at + 8 <= index.len() - 20 {
        let sig = &index[at..at + 4];
        let size = be32(&index[at + 4..at + 8]) as usize;
        let body = &index[at + 8..at + 8 + size];
        if sig == b"IEOT" {
            assert_eq!(be32(&body[..4]), 1, "IEOT_VERSION");
            return body[4..]
                .chunks_exact(8)
                .map(|c| (be32(&c[..4]), be32(&c[4..])))
                .collect();
        }
        at += 8 + size;
    }
    panic!("no IEOT extension was written");
}

#[test]
fn a_version_four_block_boundary_strips_the_whole_previous_name() {
    let f = Fixture::new("v4ieot");
    let mut args: Vec<&str> = vec![
        "-c",
        "index.threads=4",
        "-c",
        "index.recordOffsetTable=true",
        "update-index",
        "--index-version",
        "4",
        "--add",
    ];
    args.extend(f.paths.iter().map(String::as_str));
    f.ok(&args);

    let index = f.index();
    let (entries, one_past) = v4_entries(&index);
    assert_eq!(entries.len(), f.paths.len());

    // The compression has to round-trip first; a wrong strip count that happened to
    // decode back to the right name would be a different bug.
    let decoded: Vec<String> = entries
        .iter()
        .map(|e| String::from_utf8(e.path.clone()).unwrap())
        .collect();
    assert_eq!(decoded, f.paths, "version 4 paths must decode back to what was added");

    let blocks = ieot_blocks(&index, one_past);
    assert_eq!(
        blocks,
        vec![(12, 10), (725, 10), (1438, 10), (2151, 10)],
        "measured on stock git 2.55.0: `index.threads=4` over 40 entries"
    );

    // Every block's first entry must be readable without the previous block, and
    // every block's offset must be where that entry actually starts.
    for (block, &(offset, _)) in blocks.iter().enumerate() {
        let first = block * 10;
        assert_eq!(
            entries[first].offset as u32, offset,
            "block {block} claims an offset that is not entry {first}'s"
        );
        if block == 0 {
            assert_eq!(entries[0].strip, 0, "the first entry of the file has no predecessor");
            continue;
        }
        let previous = &entries[first - 1];
        assert_eq!(
            entries[first].strip,
            previous.path.len(),
            "entry {first} opens block {block} and must strip the whole of {:?}",
            String::from_utf8_lossy(&previous.path)
        );
        assert_eq!(
            entries[first].suffix, entries[first].path,
            "entry {first} opens block {block} and must carry its path in full"
        );
    }

    // Inside a block the compression is still doing its job — otherwise "strip the
    // whole previous name" could pass by never compressing anything.
    assert!(
        entries[1].strip < entries[0].path.len(),
        "entry 1 sits inside block 0 and must share a prefix with entry 0"
    );
}
