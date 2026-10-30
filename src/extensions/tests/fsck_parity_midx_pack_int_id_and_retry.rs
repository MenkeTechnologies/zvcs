//! Two multi-pack-index failures every object read has to answer for.
//!
//! A pack-int-id past the packs the midx names is `die("bad pack-int-id: %u (%u total
//! packs)")` in `midx_for_pack()` (midx.c:438-454), from whichever read reaches the object
//! through `midx_fill_entry()`; `fsck` reads a blob only when nothing else does
//! (`lookup_blob()` creates it typed, `fsck_walk()` returns for it), so a bad id on a
//! reachable blob is reported only by the `multi-pack-index verify` child.
//!
//! A midx `load_multi_pack_index_one()` gives up on with an `error()` is never stored, so
//! every `odb_source_packed_prepare()` tries it again and prints the line again: the first
//! read, `cmd_fsck()`'s `odb_reprepare()`, and one per read that misses
//! (odb.c:575-586, builtin/fsck.c:927-928).
//!
//! Stock git builds every fixture and is the oracle (`support/stock_git.rs`); the tests
//! skip when it is missing.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-fsck-midx-pid-retry-{label}-{}", std::process::id()));
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

/// Three commits of a file each, every commit repacked into a pack of its own with the
/// loose copies kept, and a midx over the three packs.
fn midx_repo(stock: &str, root: &Path, name: &str) -> PathBuf {
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
    setup(stock, &repo, &["multi-pack-index", "write"]);
    repo
}

fn midx_path(repo: &Path) -> PathBuf {
    repo.join(".git/objects/pack/multi-pack-index")
}

fn chunk(data: &[u8], id: &[u8; 4]) -> usize {
    let n = data[6] as usize;
    (0..n)
        .map(|i| 12 + 12 * i)
        .find(|&at| &data[at..at + 4] == id)
        .map(|at| u64::from_be_bytes(data[at + 4..at + 12].try_into().unwrap()) as usize)
        .expect("chunk present")
}

/// The midx position of `id`: its rank among the object names.
fn position(stock: &str, repo: &Path, id: &str) -> usize {
    let all = setup(stock, repo, &["-c", "core.multiPackIndex=false", "cat-file", "--batch-check=%(objectname)", "--batch-all-objects"]);
    all.lines().position(|l| l == id).expect("object listed")
}

/// Point the OOFF entry of `rev` past the midx's three packs.
fn bad_pack_int_id(stock: &str, root: &Path, name: &str, rev: &str) -> PathBuf {
    let repo = midx_repo(stock, root, name);
    let id = setup(stock, &repo, &["rev-parse", rev]);
    let pos = position(stock, &repo, &id);
    let mut data = std::fs::read(midx_path(&repo)).unwrap();
    let ooff = chunk(&data, b"OOFF");
    data[ooff + 8 * pos..ooff + 8 * pos + 4].copy_from_slice(&9u32.to_be_bytes());
    std::fs::write(midx_path(&repo), &data).unwrap();
    repo
}

const FSCK: [&[&str]; 7] = [
    &["fsck"],
    &["fsck", "--no-full"],
    &["fsck", "--unreachable"],
    &["fsck", "--verbose"],
    &["fsck", "--connectivity-only"],
    &["fsck", "--connectivity-only", "--verbose"],
    &["fsck", "--lost-found"],
];

#[test]
fn a_bad_pack_int_id_dies_in_the_read_that_reaches_it() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("pid");

    // A reachable blob: nothing in fsck reads it, so only the verify child reports it,
    // while every reader that does ask for it dies.
    let blob = bad_pack_int_id(stock, &root, "blob", "HEAD:f1");
    for args in FSCK {
        same(stock, &blob, args);
    }
    let (code, _, err) = same(stock, &blob, &["fsck", "--connectivity-only"]);
    assert_eq!((code, err.as_str()), (0, ""));
    for args in [&["cat-file", "-e", "HEAD:f1"][..], &["cat-file", "-p", "HEAD:f1"], &["cat-file", "-p", "HEAD"]] {
        same(stock, &blob, args);
    }
    let (code, _, err) = same(stock, &blob, &["cat-file", "-e", "HEAD:f1"]);
    assert_eq!((code, err.as_str()), (128, "fatal: bad pack-int-id: 9 (3 total packs)\n"));

    // The tip commit, which the snapshot reads, and a root tree the walk re-reads after
    // the scan freed it: fsck dies, behind the root lines its scan printed.
    for (name, rev) in [("tip", "HEAD"), ("tree", "HEAD~2^{tree}")] {
        let repo = bad_pack_int_id(stock, &root, name, rev);
        for args in FSCK.iter().copied().chain([&["fsck", "--root", "--tags"][..]]) {
            let (code, _, _) = same(stock, &repo, args);
            assert_eq!(code, 128, "{name} {args:?}");
        }
        for args in [&["cat-file", "-p", "HEAD"][..], &["cat-file", "-t", "HEAD^{tree}"], &["log", "--oneline"]] {
            same(stock, &repo, args);
        }
    }

    let _ = std::fs::remove_dir_all(&root);
}

/// Each way of making `load_multi_pack_index_one()` give up with an `error()`.
fn damaged(stock: &str, root: &Path, name: &str) -> PathBuf {
    let repo = midx_repo(stock, root, name);
    let path = midx_path(&repo);
    let mut data = std::fs::read(&path).unwrap();
    match name {
        // `multi-pack-index file %s is too small`
        "small" | "broken" => data.truncate(20),
        // `multi-pack-index hash version %u does not match version %u`
        "hash" => data[5] = 2,
        // `improper chunk offset(s) %lx and %lx`, from `read_table_of_contents()`
        _ => data[12 + 12 + 4..12 + 12 + 12].copy_from_slice(&99_999u64.to_be_bytes()),
    }
    std::fs::write(&path, &data).unwrap();
    repo
}

#[test]
fn a_midx_that_fails_to_load_is_reported_on_every_prepare() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("retry");

    let forms = FSCK.iter().copied().chain([
        &["fsck", "--progress"][..],
        &["fsck", "--connectivity-only", "--progress"],
        &["fsck", "--name-objects", "--unreachable"],
        &["fsck", "HEAD"],
        &["fsck", "main"],
        &["fsck", "nosuch"],
        &["fsck", "--connectivity-only", "main"],
    ]);
    let forms: Vec<&[&str]> = forms.collect();
    for name in ["small", "hash", "toc"] {
        let repo = damaged(stock, &root, name);
        for args in &forms {
            same(stock, &repo, args);
        }
        // The first read, `odb_reprepare()`, and the verify child's own load, which adds
        // its `failed to parse` line.
        let (code, _, err) = same(stock, &repo, &["fsck"]);
        assert_eq!(code, 1);
        assert_eq!(err.lines().count(), 4, "{err}");
    }

    // A ref to an object no source holds: `parse_object()` misses twice.
    let broken = damaged(stock, &root, "broken");
    std::fs::write(broken.join(".git/refs/heads/broken"), format!("{}\n", "12".repeat(20))).unwrap();
    for args in [&["fsck"][..], &["fsck", "--connectivity-only"], &["fsck", &"12".repeat(20)]] {
        same(stock, &broken, args);
    }

    // Two missing blobs behind a tree: `mark_object()`'s `odb_has_object()` misses on each.
    let missing = midx_repo(stock, &root, "missing");
    let tree = {
        use std::io::Write;
        let mut child = cmd(stock, &missing, &["mktree", "--missing"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(format!("100644 blob {}\tgone\n100644 blob {}\tgone2\n", "11".repeat(20), "22".repeat(20)).as_bytes())
            .unwrap();
        String::from_utf8(child.wait_with_output().unwrap().stdout).unwrap().trim().to_owned()
    };
    let commit = setup(stock, &missing, &["commit-tree", &tree, "-p", "HEAD", "-m", "missing"]);
    setup(stock, &missing, &["update-ref", "refs/heads/main", &commit]);
    let mut data = std::fs::read(midx_path(&missing)).unwrap();
    data.truncate(20);
    std::fs::write(midx_path(&missing), &data).unwrap();
    for args in [&["fsck"][..], &["fsck", "--connectivity-only"], &["fsck", "--verbose"], &["fsck", "--unreachable"]] {
        same(stock, &missing, args);
    }

    // No refs at all: the snapshot reads nothing, so `odb_reprepare()` is the first prepare.
    setup(stock, &root, &["init", "-q", "empty"]);
    let empty = root.join("empty");
    std::fs::copy(midx_path(&broken), midx_path(&empty)).unwrap();
    for args in [&["fsck"][..], &["fsck", "--connectivity-only"]] {
        same(stock, &empty, args);
    }

    let _ = std::fs::remove_dir_all(&root);
}
