//! The `.bitmap` pseudo-merge section, as `bitmapPseudoMerge.<name>.*` builds it.
//!
//! A pseudo-merge batches ref tips that were not worth a bitmap each into one
//! shared bitmap, so a reader that has already reached all of them can take
//! their whole reachable set in a single OR. The section that stores them is
//! five runs of offsets pointing at each other — the merges, a fixed-width
//! lookup table, an extended table for commits in more than one merge, a
//! position table, and a trailer — and every one of those offsets is relative
//! to a start the reader can only find by reading the trailer *backwards* from
//! the end of the file, past the lookup table and hash cache if those are
//! present.
//!
//! That makes the section unusually easy to break silently: shift any run by a
//! few bytes and the file still decodes, still passes a checksum, and answers
//! reachability questions wrongly. So this walks the section the way git's
//! `load_bitmap_header()` does — trailer first, then the tables it locates —
//! and checks that every offset lands where it should. The layered case, with
//! the hash cache and the commit lookup table also present, is the one that
//! actually pins the ordering, so it is the one exercised.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Run the binary under test in `dir`, asserting success.
fn git(dir: &Path, home: &Path, args: &[&str]) {
    let status = Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        // Old enough to be past the default `stableThreshold` of a month, so
        // every branch tip below lands in the stable half of the group.
        .env("GIT_AUTHOR_DATE", "2020-01-01T00:00:00 +0000")
        .env("GIT_COMMITTER_DATE", "2020-01-01T00:00:00 +0000")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

/// A repository with `commits` commits and a branch on each, plus an isolated
/// empty `HOME` so no ambient `bitmapPseudoMerge.*` leaks in.
fn fixture(tag: &str, commits: usize, config: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-pmbm-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    // Canonical, so that the temporary directory's symlink on macOS does not
    // make the repository path differ from the one the binary reports.
    let root = root.canonicalize().unwrap();
    let (home, repo) = (root.join("home"), root.join("repo"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();

    git(&repo, &home, &["init", "-q", "-b", "main"]);
    git(&repo, &home, &["config", "user.email", "alice@example.com"]);
    git(&repo, &home, &["config", "user.name", "Alice"]);
    for n in 0..commits {
        std::fs::write(repo.join(format!("f{n}")), format!("content {n}\n")).unwrap();
        std::fs::create_dir_all(repo.join(format!("d{n}"))).unwrap();
        std::fs::write(repo.join(format!("d{n}/nested")), format!("nested {n}\n")).unwrap();
        git(&repo, &home, &["add", "-A"]);
        git(&repo, &home, &["commit", "-q", "-m", &format!("c{n}")]);
        git(&repo, &home, &["branch", "-q", &format!("topic/{n}")]);
    }
    let mut file = std::fs::read_to_string(repo.join(".git/config")).unwrap();
    file.push_str(config);
    std::fs::write(repo.join(".git/config"), file).unwrap();
    git(&repo, &home, &["repack", "-adb", "-q"]);
    (repo, home)
}

/// The single file in `.git/objects/pack` with the given extension.
fn pack_file(repo: &Path, extension: &str) -> PathBuf {
    let mut found: Vec<PathBuf> = std::fs::read_dir(repo.join(".git/objects/pack"))
        .unwrap()
        .filter_map(|entry| {
            let path = entry.unwrap().path();
            (path.extension()? == extension).then_some(path)
        })
        .collect();
    assert_eq!(found.len(), 1, "expected exactly one .{extension}");
    found.pop().unwrap()
}

fn be32(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap())
}
fn be64(bytes: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(bytes[at..at + 8].try_into().unwrap())
}

/// What a reader recovers from the pseudo-merge section.
struct Section {
    start: usize,
    /// Byte offset of each merge, from the position table.
    merges: Vec<u64>,
    /// `(pack position, offset)` per row of the fixed-width lookup table.
    lookup: Vec<(u32, u64)>,
}

/// Walk the section back from the end of the file exactly as git's
/// `load_bitmap_header()` does: strip the checksum, then the hash cache, then
/// the commit lookup table, and only then read the pseudo-merge trailer.
fn read_section(bitmap: &[u8], objects: usize) -> Section {
    assert_eq!(&bitmap[..4], b"BITM", "signature");
    let flags = u16::from_be_bytes([bitmap[6], bitmap[7]]);
    let entries = be32(bitmap, 8) as usize;
    assert_eq!(flags & 0x20, 0x20, "BITMAP_OPT_PSEUDO_MERGES is announced");

    let mut end = bitmap.len() - 20;
    if flags & 0x4 != 0 {
        end -= 4 * objects;
    }
    if flags & 0x10 != 0 {
        end -= 16 * entries;
    }

    let start = end - be64(bitmap, end - 8) as usize;
    let lookup_at = start + be64(bitmap, end - 16) as usize;
    let commits_nr = be32(bitmap, end - 20) as usize;
    let merges_nr = be32(bitmap, end - 24) as usize;

    let positions_at = end - 24 - merges_nr * 8;
    Section {
        start,
        merges: (0..merges_nr).map(|n| be64(bitmap, positions_at + n * 8)).collect(),
        lookup: (0..commits_nr)
            .map(|n| (be32(bitmap, lookup_at + n * 12), be64(bitmap, lookup_at + n * 12 + 4)))
            .collect(),
    }
}

/// Objects in a v2 `.idx`, from the last fanout bucket.
fn objects_in_pack(idx: &[u8]) -> usize {
    assert_eq!(&idx[..4], &[0xff, b't', b'O', b'c'], "a v2 index");
    be32(idx, 8 + 255 * 4) as usize
}

/// One branch per commit and `stableSize = 4` cuts 21 tips into six merges, of
/// which the first five take four tips each. That the counts come out exactly
/// so is what pins the partitioning; that every offset resolves is what pins the
/// layout.
#[test]
fn every_offset_in_the_section_resolves() {
    let (repo, _home) = fixture(
        "offsets",
        20,
        "[bitmapPseudoMerge \"g\"]\n\tpattern = refs/heads/\n\tstableSize = 4\n",
    );
    let bitmap = std::fs::read(pack_file(&repo, "bitmap")).unwrap();
    let objects = objects_in_pack(&std::fs::read(pack_file(&repo, "idx")).unwrap());
    let section = read_section(&bitmap, objects);

    assert_eq!(
        section.merges.len(),
        6,
        "twenty topic branches plus main, four to a merge"
    );
    assert_eq!(section.lookup.len(), 20, "one row per commit that is a parent");

    assert!(
        section.merges.windows(2).all(|pair| pair[0] < pair[1]),
        "merges are written in the order the position table names them"
    );
    assert!(
        section.merges[0] as usize >= section.start,
        "the first merge is inside the section"
    );

    // A reader binary-searches the lookup table, which only works if it ascends.
    assert!(
        section.lookup.windows(2).all(|pair| pair[0].0 < pair[1].0),
        "lookup rows ascend by pack position"
    );

    let flag = 1u64 << 63;
    for (position, offset) in &section.lookup {
        if offset & flag == 0 {
            assert!(
                section.merges.contains(offset),
                "row for position {position} points at {offset}, which is not a merge"
            );
            continue;
        }
        // An extended row points at a count followed by that many merge offsets.
        let at = (offset & !flag) as usize;
        assert!(at < bitmap.len(), "extended offset for {position} is past the file");
        let count = be32(&bitmap, at) as usize;
        assert!(count > 1, "a commit only goes extended when several merges hold it");
        for n in 0..count {
            let target = be64(&bitmap, at + 4 + n * 8);
            assert!(
                section.merges.contains(&target),
                "extended row for position {position} points at {target}, which is not a merge"
            );
        }
    }
}

/// The section sits ahead of the commit lookup table and the hash cache, and a
/// reader finds it only by stripping those two first. Writing it in the wrong
/// place still produces a file that decodes, so the ordering needs its own
/// check with both of those sections present.
#[test]
fn the_section_is_found_behind_the_hash_cache_and_lookup_table() {
    let (repo, _home) = fixture(
        "layered",
        20,
        "[bitmapPseudoMerge \"g\"]\n\tpattern = refs/heads/\n\tstableSize = 4\n\
         [pack]\n\twriteBitmapHashCache = true\n\twriteBitmapLookupTable = true\n",
    );
    let bitmap = std::fs::read(pack_file(&repo, "bitmap")).unwrap();
    let objects = objects_in_pack(&std::fs::read(pack_file(&repo, "idx")).unwrap());

    let flags = u16::from_be_bytes([bitmap[6], bitmap[7]]);
    assert_eq!(flags & 0x4, 0x4, "the hash cache is present");
    assert_eq!(flags & 0x10, 0x10, "the commit lookup table is present");

    let section = read_section(&bitmap, objects);
    assert_eq!(section.merges.len(), 6, "the same six merges as without them");
    assert!(
        section.lookup.windows(2).all(|pair| pair[0].0 < pair[1].0),
        "lookup rows still ascend once the section is behind two others"
    );
    for (position, offset) in &section.lookup {
        if offset & 1 << 63 == 0 {
            assert!(
                section.merges.contains(offset),
                "row for position {position} resolves once the trailing sections are stripped"
            );
        }
    }
}

/// A group whose pattern matches nothing, and a repository with no group at
/// all, both have to leave the flag clear — a section header claiming merges
/// that are not there is a file git rejects outright.
#[test]
fn no_matching_tips_means_no_section() {
    for (tag, config) in [
        ("nogroup", String::new()),
        (
            "nomatch",
            "[bitmapPseudoMerge \"g\"]\n\tpattern = refs/nothing/\n".to_string(),
        ),
    ] {
        let (repo, _home) = fixture(tag, 4, &config);
        let bitmap = std::fs::read(pack_file(&repo, "bitmap")).unwrap();
        let flags = u16::from_be_bytes([bitmap[6], bitmap[7]]);
        assert_eq!(flags & 0x20, 0, "{tag}: no merges, so no section and no flag");
    }
}
