//! Reads of a pack entry git cannot use: `packfile.c`'s own diagnostics, the copy it falls back
//! to, and the corrupt answer when there is none.
//!
//! `packed_object_info()` reads an entry through `unpack_object_header()`,
//! `packed_to_object_type()` and `unpack_entry()`, which print `unknown object type %i at offset
//! %u in %s` and `git_inflate()`'s `inflate:` line for what they cannot use (packfile.c:1080-1145,
//! 1503-1720). `odb_source_packed_read_object_info()` then marks the object bad in that pack
//! (odb/source-packed.c:117-127), the loose copy is tried, and the second read of
//! `do_oid_object_info_extended()` passes over the bad copy (packfile.c:1846-1853,
//! midx.c:621-626). With no copy left the read is `packed object %s (stored in %s) is corrupt`,
//! which `odb_read_object()` dies with (odb.c:597-603) and `odb_read_object_info()` returns as a
//! plain failure; `cat-file -s` asks for the size alone and never checks the type
//! (packfile.c:1330-1350).
//!
//! A bad offset in a multi-pack-index reaches the same code: the midx names the pack, the
//! offset lands on bytes that are no object header. `fsck`'s object scan reads every pack
//! through the pack's own `.idx` and never sees it; the reachability walk after it does.
//!
//! Stock git builds every fixture and is the oracle (`support/stock_git.rs`); the tests skip
//! when it is missing.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-corrupt-packed-entry-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir).unwrap()
}

fn cmd(bin: &str, dir: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(bin);
    c.args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1112911993 -0700")
        .env("GIT_COMMITTER_DATE", "1112911993 -0700")
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null());
    c
}

fn setup(stock: &str, dir: &Path, args: &[&str]) -> String {
    let out = cmd(stock, dir, args).output().unwrap();
    assert!(out.status.success(), "setup {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn run(bin: &str, dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = cmd(bin, dir, args).output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[track_caller]
fn same(stock: &str, dir: &Path, args: &[&str]) -> (i32, String, String) {
    let theirs = run(stock, dir, args);
    let ours = run(ZVCS, dir, args);
    assert_eq!(ours, theirs, "git {args:?} in {} (left zvcs, right stock)", dir.display());
    theirs
}

/// Three commits of a file each, each repacked into a pack of its own; `keep_loose` leaves the
/// loose copies in place, `midx` writes a multi-pack-index over the three packs.
fn three_packs(stock: &str, root: &Path, name: &str, keep_loose: bool, midx: bool) -> PathBuf {
    setup(stock, root, &["init", "-q", "-b", "main", name]);
    let repo = root.join(name);
    setup(stock, &repo, &["config", "gc.auto", "0"]);
    setup(stock, &repo, &["config", "maintenance.auto", "false"]);
    for i in 0..3 {
        std::fs::write(repo.join(format!("f{i}")), format!("file {i}\n")).unwrap();
        setup(stock, &repo, &["add", "-A"]);
        setup(stock, &repo, &["commit", "-q", "-m", &format!("c{i}")]);
        setup(stock, &repo, &["repack", "-q"]);
    }
    if !keep_loose {
        setup(stock, &repo, &["prune-packed"]);
    }
    if midx {
        setup(stock, &repo, &["multi-pack-index", "write"]);
    }
    repo
}

/// The pack holding `id` and the offset of `id` in it, from each pack's own index.
fn pack_offset(stock: &str, repo: &Path, id: &str) -> (PathBuf, u64) {
    let dir = repo.join(".git/objects/pack");
    for entry in std::fs::read_dir(&dir).unwrap() {
        let idx = entry.unwrap().path();
        if idx.extension().is_none_or(|x| x != "idx") {
            continue;
        }
        let out = Command::new(stock)
            .arg("show-index")
            .stdin(std::fs::File::open(&idx).unwrap())
            .output()
            .unwrap();
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let mut f = line.split_whitespace();
            let (Some(offset), Some(name)) = (f.next(), f.next()) else { continue };
            if name == id {
                return (idx.with_extension("pack"), offset.parse().unwrap());
            }
        }
    }
    panic!("{id} is in no pack");
}

fn poke(path: &Path, at: u64, bytes: &[u8]) {
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    std::fs::set_permissions(path, perms).unwrap();
    let mut data = std::fs::read(path).unwrap();
    data[at as usize..at as usize + bytes.len()].copy_from_slice(bytes);
    std::fs::write(path, data).unwrap();
}

/// Point the midx's OOFF entry for `id` at offset 5 of its pack, inside the pack header.
fn bad_midx_offset(stock: &str, repo: &Path, id: &str) {
    let all = setup(stock, repo, &["-c", "core.multiPackIndex=false", "cat-file", "--batch-check=%(objectname)", "--batch-all-objects"]);
    let pos = all.lines().position(|l| l == id).expect("object listed");
    let path = repo.join(".git/objects/pack/multi-pack-index");
    let data = std::fs::read(&path).unwrap();
    let n = data[6] as usize;
    let ooff = (0..n)
        .map(|i| 12 + 12 * i)
        .find(|&at| &data[at..at + 4] == b"OOFF")
        .map(|at| u64::from_be_bytes(data[at + 4..at + 12].try_into().unwrap()))
        .expect("OOFF chunk");
    poke(&path, ooff + 8 * pos as u64 + 4, &5u32.to_be_bytes());
}

/// The reads every fixture is compared under. A full `fsck` over a damaged pack is left out where
/// the pack itself is damaged: `verify_pack()` failing is `ERROR_OBJECT` in git 2.56
/// (odb/source-packed.c:912-918, builtin/fsck.c:933-935), which this port still reports as the
/// `ERROR_PACK` of older releases, a gap of its own.
fn every_read(stock: &str, repo: &Path, ids: &[&str], extra: &[&[&str]]) {
    for id in ids {
        for mode in ["-p", "-t", "-s", "-e"] {
            same(stock, repo, &["cat-file", mode, id]);
        }
    }
    for args in [&["rev-list", "--objects", "--all"][..], &["fsck", "--connectivity-only"], &["fsck", "--no-full"]] {
        same(stock, repo, args);
    }
    for args in extra {
        same(stock, repo, args);
    }
}

#[test]
fn a_type_bit_pattern_git_does_not_know_is_reported_and_the_object_is_corrupt() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("type");
    let repo = three_packs(stock, &root, "type", false, false);
    let tree = setup(stock, &repo, &["rev-parse", "HEAD~2^{tree}"]);
    let (pack, offset) = pack_offset(stock, &repo, &tree);
    // Type 0 in the header byte, size bits kept.
    let data = std::fs::read(&pack).unwrap();
    poke(&pack, offset, &[data[offset as usize] & 0x8f]);
    let (code, _, err) = same(stock, &repo, &["cat-file", "-t", &tree]);
    assert_eq!(code, 128);
    assert!(err.starts_with("error: unknown object type 0 at offset"), "{err}");
    let (code, _, err) = same(stock, &repo, &["rev-list", "--objects", "--all"]);
    assert_eq!(code, 128);
    assert!(err.contains(") is corrupt\n"), "{err}");
    every_read(stock, &repo, &[&tree], &[]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_broken_zlib_stream_is_reported_by_inflate() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("zlib");
    let repo = three_packs(stock, &root, "zlib", false, false);
    let commit = setup(stock, &repo, &["rev-parse", "HEAD~1"]);
    let (pack, offset) = pack_offset(stock, &repo, &commit);
    // The first byte past the two-byte object header is the zlib CMF byte.
    poke(&pack, offset + 2, &[0x00]);
    let (code, _, err) = same(stock, &repo, &["cat-file", "-p", &commit]);
    assert_eq!(code, 128);
    assert!(err.starts_with("error: inflate: "), "{err}");
    every_read(stock, &repo, &[&commit], &[&["log", "--stat", "--format=%s"]]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_loose_copy_answers_for_a_corrupt_packed_one() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("loose");
    let repo = three_packs(stock, &root, "loose", true, false);
    let tree = setup(stock, &repo, &["rev-parse", "HEAD~2^{tree}"]);
    let (pack, offset) = pack_offset(stock, &repo, &tree);
    let data = std::fs::read(&pack).unwrap();
    poke(&pack, offset, &[data[offset as usize] & 0x8f]);
    let (code, out, _) = same(stock, &repo, &["cat-file", "-p", &tree]);
    assert_eq!((code, out.is_empty()), (0, false));
    every_read(stock, &repo, &[&tree], &[&["log", "--stat", "--format=%s"]]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_bad_multi_pack_index_offset_reads_the_pack_header() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("midx");
    for keep_loose in [false, true] {
        let name = format!("midx-{keep_loose}");
        let repo = three_packs(stock, &root, &name, keep_loose, true);
        let tree = setup(stock, &repo, &["rev-parse", "HEAD~2^{tree}"]);
        bad_midx_offset(stock, &repo, &tree);
        // The header byte at offset 5 is the version's high byte: type 0, size 0, which `-s`
        // prints without checking the type.
        if !keep_loose {
            let (code, out, _) = same(stock, &repo, &["cat-file", "-s", &tree]);
            assert_eq!((code, out.as_str()), (0, "0\n"));
        }
        every_read(stock, &repo, &[&tree], &[&["fsck"], &["fsck", "--unreachable"]]);
        if keep_loose {
            same(stock, &repo, &["log", "--stat", "--format=%s"]);
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_delta_over_a_broken_base_fails_after_the_base_does() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("delta");
    for broken_type in [false, true] {
        let name = format!("delta-{broken_type}");
        setup(stock, &root, &["init", "-q", "-b", "main", &name]);
        let repo = root.join(&name);
        // Five versions of `f`, growing: the last is stored whole and the four before it are
        // deltas on it.
        for i in 1..=5 {
            let body: String = (1..=i * 200).map(|n| format!("{n}\n")).collect();
            std::fs::write(repo.join("f"), body).unwrap();
            setup(stock, &repo, &["add", "f"]);
            setup(stock, &repo, &["commit", "-q", "-m", &format!("c{i}")]);
        }
        setup(stock, &repo, &["repack", "-adq"]);
        let base = setup(stock, &repo, &["rev-parse", "HEAD:f"]);
        let delta = setup(stock, &repo, &["rev-parse", "HEAD~1:f"]);
        let (pack, offset) = pack_offset(stock, &repo, &base);
        let data = std::fs::read(&pack).unwrap();
        if broken_type {
            // `packed_to_object_type()` meets the base's type 0 and retries it and then the delta
            // through the object database, which marks both bad without a line of its own.
            poke(&pack, offset, &[data[offset as usize] & 0x8f]);
            let (code, _, err) = same(stock, &repo, &["cat-file", "-t", &delta]);
            assert_eq!((code, err.as_str()), (128, "fatal: git cat-file: could not get object info\n"));
        } else {
            // Inside the base's deflate stream: the header still types the delta, and the content
            // read reports the inflate, then the base it could not read.
            poke(&pack, offset + 6, &[0x00]);
            let (code, _, err) = same(stock, &repo, &["cat-file", "-p", &delta]);
            assert_eq!(code, 128);
            assert!(err.contains("error: failed to read delta base object "), "{err}");
        }
        every_read(stock, &repo, &[&base, &delta], &[]);
    }
    let _ = std::fs::remove_dir_all(&root);
}
