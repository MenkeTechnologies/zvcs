//! `IEOT` — the index-entry offset table — reaches *every* verb that writes an
//! index, exactly like [`index.skipHash`](../index_skip_hash_per_verb.rs) does.
//!
//! git has no per-command switch for it either. `do_write_index()` evaluates
//! `if (nr_threads != 1 && record_ieot())` before it serialises a single entry
//! (read-cache.c:2877-2904), because the block boundaries have to be recorded
//! while the entries are written, and then emits the extension ahead of all the
//! others (`:2983-2993`). Every writer in the C reaches that through
//! `write_locked_index()` (read-cache.c:3323), so an index written by `add`
//! cannot carry fewer extensions than one written by `update-index`.
//!
//! In the port that decision lives outside the writer
//! (`porcelain/write_tree.rs::prepare_offset_table`), so it is a line each verb
//! has to attach. `add` and `stage` — one `cmd_add` in git — were not attaching
//! it, which cost them `IEOT` *and* `EOIE`: gix only emits the end-of-index-entry
//! extension alongside another one, so dropping the offset table dropped both.
//!
//! Every expectation below was taken from a differential run against stock git
//! 2.55.0 in the same fixture (`/opt/homebrew/bin/git -c index.threads=4 <verb> …`
//! then a byte-level read of `.git/index`); the tests shell out to nothing but the
//! binary under test. `update-index` is included as the control: it already
//! matched, and it is what `add`/`stage` now have to agree with.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(repo: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .env("LC_ALL", "C")
        .output()
        .unwrap()
}

fn ok(repo: &Path, home: &Path, args: &[&str]) -> Output {
    let out = run(repo, home, args);
    assert!(
        out.status.success(),
        "`git {args:?}` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// Eight tracked-to-be files: enough that `-c index.threads=4` yields four blocks
/// (`ieot_blocks = nr_threads`, `ieot_entries = DIV_ROUND_UP(8, 4) = 2`) and the
/// `ieot_blocks > 1` guard passes.
const FILES: usize = 8;

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-ieot-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        let home = root.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let f = Fixture { root, repo, home };
        ok(&f.repo, &f.home, &["init", "-q", "-b", "main", "."]);
        for i in 0..FILES {
            std::fs::write(f.repo.join(format!("f{i}.txt")), format!("f{i}\n")).unwrap();
        }
        f
    }

    fn git(&self, args: &[&str]) -> Output {
        ok(&self.repo, &self.home, args)
    }

    fn index(&self) -> Vec<u8> {
        std::fs::read(self.repo.join(".git/index")).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// One extension as it appears in the index: signature and payload length.
#[derive(Debug, PartialEq, Eq)]
struct Ext {
    sig: String,
    size: usize,
    /// Offset of the payload's first byte, for the checks that read into it.
    at: usize,
}

fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
}

/// Walk the index's entries and return the extension chain that follows them.
///
/// The entry walk is the on-disk layout for index versions 2 and 3: 62 fixed
/// bytes, two more when the entry is `EXTENDED` (`CE_EXTENDED`, flag `0x4000`),
/// then a NUL-terminated path padded so each entry starts 8-byte aligned. Version
/// 4's path compression is not handled because nothing here writes v4.
fn extensions(index: &[u8]) -> Vec<Ext> {
    assert_eq!(&index[..4], b"DIRC", "not an index file");
    let version = be32(index, 4);
    assert!(version == 2 || version == 3, "unexpected index version {version}");
    let entries = be32(index, 8) as usize;

    let mut off = 12;
    for _ in 0..entries {
        let start = off;
        let flags = u16::from_be_bytes(index[off + 60..off + 62].try_into().unwrap());
        let mut p = off + 62;
        if flags & 0x4000 != 0 {
            p += 2;
        }
        let nul = index[p..].iter().position(|b| *b == 0).expect("unterminated path") + p;
        off = nul + 1;
        while (off - start) % 8 != 0 {
            off += 1;
        }
    }

    let mut out = Vec::new();
    // The last 20 bytes are the trailing hash, never an extension header.
    while off < index.len() - 20 {
        let sig = String::from_utf8(index[off..off + 4].to_vec()).unwrap();
        let size = be32(index, off + 4) as usize;
        out.push(Ext { sig, size, at: off + 8 });
        off += 8 + size;
    }
    out
}

fn signatures(index: &[u8]) -> Vec<String> {
    extensions(index).into_iter().map(|e| e.sig).collect()
}

/// `-c index.threads=4`, the setting every positive case below is written under.
fn threaded<'a>(rest: &[&'a str]) -> Vec<&'a str> {
    let mut v = vec!["-c", "index.threads=4"];
    v.extend_from_slice(rest);
    v
}

#[test]
fn add_writes_the_offset_table() {
    let f = Fixture::new("add");
    f.git(&threaded(&["add", "."]));
    assert_eq!(signatures(&f.index()), ["IEOT", "EOIE"]);
}

#[test]
fn add_intent_to_add_writes_the_offset_table() {
    let f = Fixture::new("addN");
    f.git(&threaded(&["add", "-N", "."]));
    let index = f.index();
    assert_eq!(be32(&index, 4), 3, "intent-to-add entries force index version 3");
    assert_eq!(signatures(&index), ["IEOT", "EOIE"]);
}

/// `stage` is registered as `cmd_add` itself, so it has no index-writing
/// behaviour of its own to diverge with.
#[test]
fn stage_writes_the_offset_table() {
    let f = Fixture::new("stage");
    f.git(&threaded(&["stage", "."]));
    assert_eq!(signatures(&f.index()), ["IEOT", "EOIE"]);
}

#[test]
fn stage_intent_to_add_writes_the_offset_table() {
    let f = Fixture::new("stageN");
    f.git(&threaded(&["stage", "-N", "."]));
    assert_eq!(signatures(&f.index()), ["IEOT", "EOIE"]);
}

/// `--refresh` writes back through the same `write_locked_index()` as a staging
/// run, so a refreshed index may not come out with fewer extensions than a staged
/// one — and here it also has to keep the cache-tree the commit left behind.
#[test]
fn stage_refresh_writes_the_offset_table() {
    let f = Fixture::new("refresh");
    f.git(&["-c", "index.threads=1", "add", "."]);
    f.git(&["-c", "index.threads=1", "commit", "-q", "-m", "c0"]);
    assert_eq!(signatures(&f.index()), ["TREE"], "fixture should start with a cache-tree only");

    // Past the index's own timestamp, so the re-stat is a real change rather than
    // a racily-clean one the refresh is entitled to ignore.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    for i in 0..FILES {
        let path = f.repo.join(format!("f{i}.txt"));
        let content = std::fs::read(&path).unwrap();
        std::fs::write(&path, content).unwrap();
    }

    f.git(&threaded(&["stage", "--refresh", "."]));
    assert_eq!(signatures(&f.index()), ["IEOT", "TREE", "EOIE"]);
}

/// The control: the plumbing verb that already agreed with stock. If this ever
/// disagrees with the porcelain cases above, the shared decision has drifted.
#[test]
fn update_index_writes_the_offset_table() {
    let f = Fixture::new("updidx");
    let names: Vec<String> = (0..FILES).map(|i| format!("f{i}.txt")).collect();
    let mut args = threaded(&["update-index", "--add"]);
    args.extend(names.iter().map(String::as_str));
    f.git(&args);
    assert_eq!(signatures(&f.index()), ["IEOT", "EOIE"]);
}

/// The offset table is not just present, it describes the entries that were
/// actually written: four blocks of two for eight entries under four threads, the
/// first starting where the entries do (right after the 12-byte header).
#[test]
fn the_offset_table_describes_the_entries_it_was_written_with() {
    let f = Fixture::new("payload");
    f.git(&threaded(&["add", "."]));
    let index = f.index();
    let exts = extensions(&index);
    let ieot = exts.iter().find(|e| e.sig == "IEOT").expect("no IEOT");

    // `write_ieot_extension()`: a 4-byte version, then one (offset, nr) pair per
    // block. `ieot_blocks = nr_threads = 4`, `ieot_entries = DIV_ROUND_UP(8, 4)`.
    assert_eq!(ieot.size, 4 + 4 * 8, "expected four 8-byte block records");
    assert_eq!(be32(&index, ieot.at), 1, "IEOT version");
    assert_eq!(be32(&index, ieot.at + 4), 12, "first block starts at the end of the header");
    for block in 0..4 {
        let nr = be32(&index, ieot.at + 4 + block * 8 + 4);
        assert_eq!(nr, 2, "block {block} should hold DIV_ROUND_UP(8, 4) entries");
    }
}

/// Unconfigured, `record_ieot()` answers "was threading explicitly asked for",
/// which it was not — so neither verb writes the extension, and `EOIE` goes with
/// it.
#[test]
fn without_index_threads_neither_verb_writes_it() {
    for verb in ["add", "stage"] {
        let f = Fixture::new(&format!("unset-{verb}"));
        f.git(&[verb, "."]);
        assert!(
            signatures(&f.index()).is_empty(),
            "{verb} wrote an extension with index.threads unset"
        );
    }
}

/// `if (nr_threads != 1 && record_ieot())` — the thread count is checked *first*,
/// so a single-threaded index has no offset table however explicitly it was asked
/// for.
#[test]
fn one_thread_suppresses_the_offset_table_even_when_requested() {
    let f = Fixture::new("t1");
    f.git(&["-c", "index.threads=1", "-c", "index.recordOffsetTable=true", "add", "."]);
    assert!(signatures(&f.index()).is_empty(), "index.threads=1 must suppress IEOT");
}

/// The two extensions are governed by two keys. Turning `EOIE` off leaves the
/// offset table alone, which is also the case that proves `IEOT` is written on its
/// own account rather than as a side effect of the end-of-index-entry decision.
#[test]
fn the_offset_table_survives_end_of_index_entries_being_turned_off() {
    let f = Fixture::new("noeoie");
    f.git(&threaded(&["-c", "index.recordEndOfIndexEntries=false", "add", "."]));
    assert_eq!(signatures(&f.index()), ["IEOT"]);
}

/// One repository, one answer: the extension chain an index carries must not
/// depend on which verb wrote it.
#[test]
fn one_repository_gets_one_extension_chain_whichever_verb_wrote_it() {
    let names: Vec<String> = (0..FILES).map(|i| format!("f{i}.txt")).collect();

    let by_add = {
        let f = Fixture::new("same-add");
        f.git(&threaded(&["add", "."]));
        signatures(&f.index())
    };
    let by_stage = {
        let f = Fixture::new("same-stage");
        f.git(&threaded(&["stage", "."]));
        signatures(&f.index())
    };
    let by_update_index = {
        let f = Fixture::new("same-updidx");
        let mut args = threaded(&["update-index", "--add"]);
        args.extend(names.iter().map(String::as_str));
        f.git(&args);
        signatures(&f.index())
    };

    assert_eq!(by_add, by_update_index, "add disagrees with update-index");
    assert_eq!(by_stage, by_update_index, "stage disagrees with update-index");
}
