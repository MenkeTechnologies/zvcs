//! `fsck` over a multi-pack-index that covers a pack `is_pack_valid()` refuses, and over a
//! multi-pack-index whose offset for one object lies beyond the end of its pack.
//!
//! `midx_fill_entry()` loads the pack that owns an object through `prepare_midx_pack()` before
//! `is_pack_valid()` refuses it (midx.c:456-485, :592-631), and only a read's second pass looks
//! for the object in the other covered packs (odb/source-packed.c:48-73). So a snapshot read the
//! refused pack alone could answer still appends packs to the list the scan walks, and
//! `has_object_pack()`, which makes no second pass (packfile.c:1922-1933), finds a head
//! `missing` that `parse_object()` read. A commit or tag the scan parsed is walked from that
//! parse with no read, and a cache-tree node `parse_object()` read keeps its buffer, so the walk
//! reads again only the trees `fsck_obj()` freed (builtin/fsck.c:446-448, object.c:335-337).
//!
//! A refused pack makes the full scan's exit code `ERROR_PACK` in this port and `ERROR_OBJECT` in
//! git 2.56, a pending owner decision (see fsck_parity_corrupt_pack.rs), so the scans that verify
//! packs compare stdout and stderr only.
//!
//! Stock git builds every fixture and is the oracle (`support/stock_git.rs`); the tests skip
//! when it is missing.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-fsck-midx-refused-{label}-{}", std::process::id()));
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
        .stdin(Stdio::null());
    c
}

fn setup(stock: &str, dir: &Path, args: &[&str]) -> String {
    let out = cmd(stock, dir, args).output().unwrap();
    assert!(out.status.success(), "setup {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn setup_with_input(stock: &str, dir: &Path, args: &[&str], input: &[u8]) {
    let mut child = cmd(stock, dir, args).stdin(Stdio::piped()).stdout(Stdio::null()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    assert!(child.wait().unwrap().success(), "setup {args:?}");
}

fn run(bin: &str, dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = cmd(bin, dir, args).output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// stdout, stderr and the exit code.
#[track_caller]
fn same(stock: &str, dir: &Path, args: &[&str]) -> (i32, String, String) {
    let theirs = run(stock, dir, args);
    let ours = run(ZVCS, dir, args);
    assert_eq!(ours, theirs, "git {args:?} in {} (left zvcs, right stock)", dir.display());
    theirs
}

/// stdout and stderr, for a scan whose exit code is the pending `ERROR_PACK` decision.
#[track_caller]
fn same_output(stock: &str, dir: &Path, args: &[&str]) -> (String, String) {
    let (_, out, err) = run(stock, dir, args);
    let (_, ours_out, ours_err) = run(ZVCS, dir, args);
    assert_eq!((ours_out, ours_err), (out.clone(), err.clone()), "git {args:?} in {} (left zvcs, right stock)", dir.display());
    (out, err)
}

fn writable(path: &Path) {
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    std::fs::set_permissions(path, perms).unwrap();
}

fn set_mtime(path: &Path, secs: u64) {
    writable(path);
    let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs)).unwrap();
}

fn packs(repo: &Path) -> Vec<PathBuf> {
    let mut packs: Vec<PathBuf> = std::fs::read_dir(repo.join(".git/objects/pack"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "pack"))
        .collect();
    packs.sort();
    packs
}

/// What the truncated pack holds besides what the good pack holds.
#[derive(Clone, Copy, Debug)]
enum Shape {
    /// The root commit's objects again: every object is also in the good pack.
    Dup,
    /// An annotated tag of the root commit and that commit's objects, in the truncated pack only.
    Tag,
    /// The tip commit, in the truncated pack only.
    Head,
}

/// Which `.pack` is younger, which both `sort_pack()` and the midx's choice of owner for a
/// duplicate follow.
#[derive(Clone, Copy, Debug)]
enum Age {
    Tie,
    BadOlder,
    BadNewer,
}

/// Three commits over files in subdirectories, packed into a good pack and a pack truncated to
/// 200 bytes, which `is_pack_valid()` refuses, both covered by a multi-pack-index.
fn refused_owner(stock: &str, root: &Path, shape: Shape, age: Age) -> PathBuf {
    let name = format!("{shape:?}-{age:?}");
    setup(stock, root, &["init", "-q", "-b", "main", &name]);
    let repo = root.join(&name);
    for i in 0..3 {
        std::fs::write(repo.join(format!("f{i}")), format!("f{i}\n")).unwrap();
        std::fs::create_dir_all(repo.join(format!("d{}", i % 3))).unwrap();
        std::fs::write(repo.join(format!("d{}/g{i}", i % 3)), format!("g{i}\n")).unwrap();
        setup(stock, &repo, &["add", "-A"]);
        setup(stock, &repo, &["commit", "-q", "-m", &format!("c{i}")]);
    }
    let c0 = setup(stock, &repo, &["rev-list", "--max-parents=0", "HEAD"]);
    let root_objects = |repo: &Path| -> Vec<String> {
        let mut ids = vec![c0.clone(), setup(stock, repo, &["rev-parse", &format!("{c0}^{{tree}}")])];
        let listing = setup(stock, repo, &["ls-tree", "-r", "-t", &c0]);
        ids.extend(listing.lines().map(|l| l.split_whitespace().nth(2).unwrap().to_owned()));
        ids
    };
    let bad: Vec<String> = match shape {
        Shape::Dup => root_objects(&repo),
        Shape::Tag => {
            setup(stock, &repo, &["tag", "-a", "-m", "tagged", "v1", &c0]);
            setup(stock, &repo, &["tag", "-a", "-m", "t2", "v2", "HEAD~1"]);
            let mut ids = vec![setup(stock, &repo, &["rev-parse", "v1"])];
            ids.extend(root_objects(&repo));
            ids
        }
        Shape::Head => vec![setup(stock, &repo, &["rev-parse", "HEAD"])],
    };
    setup_with_input(stock, &repo, &["pack-objects", "-q", ".git/objects/pack/pack"], format!("{}\n", bad.join("\n")).as_bytes());
    let bad_pack = packs(&repo).pop().unwrap();
    let all = setup(stock, &repo, &["rev-list", "--objects", "--all"]);
    let rest: Vec<&str> = all
        .lines()
        .map(|l| &l[..40])
        .filter(|id| matches!(shape, Shape::Dup) || !bad.iter().any(|b| b == id))
        .collect();
    setup_with_input(stock, &repo, &["pack-objects", "-q", ".git/objects/pack/pack"], format!("{}\n", rest.join("\n")).as_bytes());
    setup(stock, &repo, &["prune-packed"]);
    let good_pack = packs(&repo).into_iter().find(|p| *p != bad_pack).unwrap();
    writable(&bad_pack);
    std::fs::OpenOptions::new().write(true).open(&bad_pack).unwrap().set_len(200).unwrap();
    let (bad_at, good_at) = match age {
        Age::Tie => (1_577_836_800, 1_577_836_800),
        Age::BadOlder => (1_577_836_800, 1_609_459_200),
        Age::BadNewer => (1_609_459_200, 1_577_836_800),
    };
    set_mtime(&bad_pack, bad_at);
    set_mtime(&good_pack, good_at);
    // The truncated pack makes the writer's own reads complain; the file is still written.
    let _ = run(stock, &repo, &["multi-pack-index", "write"]);
    assert!(repo.join(".git/objects/pack/multi-pack-index").is_file());
    repo
}

#[test]
fn a_midx_owner_is_pack_valid_refuses_is_read_as_git_reads_it() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("owner");
    for shape in [Shape::Dup, Shape::Tag, Shape::Head] {
        for age in [Age::Tie, Age::BadOlder, Age::BadNewer] {
            let repo = refused_owner(stock, &root, shape, age);
            let (_, err) = same_output(stock, &repo, &["fsck", "--verbose"]);
            assert!(err.contains(" does not match index\n"), "{shape:?} {age:?}: {err}");
            same_output(stock, &repo, &["fsck"]);
            same_output(stock, &repo, &["fsck", "--verbose", "--root", "--unreachable", "--tags", "--name-objects"]);
            same_output(stock, &repo, &["fsck", "--lost-found"]);
            same_output(stock, &repo, &["fsck", "--dangling", "--no-reflogs"]);
            // Neither verifies a pack, so the exit codes agree too.
            let (_, out, _) = same(stock, &repo, &["fsck", "--no-full", "--verbose"]);
            if let (Shape::Dup, Age::BadNewer) = (shape, age) {
                // The cache-tree node the midx gives the refused pack is `missing`.
                assert!(out.contains("missing tree "), "{out}");
            }
            same(stock, &repo, &["fsck", "--no-full", "--unreachable", "--verbose"]);
            // `--connectivity-only`'s slot order with the refused pack the newer of two holding
            // the same objects is not settled here.
            if !matches!((shape, age), (Shape::Dup, Age::BadNewer)) {
                same(stock, &repo, &["fsck", "--connectivity-only", "--verbose"]);
                same(stock, &repo, &["fsck", "--connectivity-only", "--unreachable", "--verbose"]);
            }
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// Set the multi-pack-index's OOFF entry for `id` to `value`.
fn set_midx_offset(stock: &str, repo: &Path, id: &str, value: u32) {
    let all = setup(stock, repo, &["-c", "core.multiPackIndex=false", "cat-file", "--batch-check=%(objectname)", "--batch-all-objects"]);
    let pos = all.lines().position(|l| l == id).expect("object listed");
    let path = repo.join(".git/objects/pack/multi-pack-index");
    let mut data = std::fs::read(&path).unwrap();
    let chunks = data[6] as usize;
    let ooff = (0..chunks)
        .map(|i| 12 + 12 * i)
        .find(|&at| &data[at..at + 4] == b"OOFF")
        .map(|at| u64::from_be_bytes(data[at + 4..at + 12].try_into().unwrap()) as usize)
        .expect("OOFF chunk");
    let at = ooff + 8 * pos + 4;
    data[at..at + 4].copy_from_slice(&value.to_be_bytes());
    writable(&path);
    std::fs::write(&path, data).unwrap();
}

#[test]
fn a_midx_offset_beyond_the_pack_dies_only_where_git_reads() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("big");
    for (name, rev) in [
        ("c0", "HEAD~2"),
        ("c1", "HEAD~1"),
        ("head", "HEAD"),
        ("tree", "HEAD^{tree}"),
        ("t0", "HEAD~2^{tree}"),
        ("t1", "HEAD~1^{tree}"),
        ("blob", "HEAD:f1"),
    ] {
        setup(stock, &root, &["init", "-q", "-b", "main", name]);
        let repo = root.join(name);
        setup(stock, &repo, &["config", "gc.auto", "0"]);
        for i in 0..3 {
            std::fs::write(repo.join(format!("f{i}")), format!("file {i}\n")).unwrap();
            setup(stock, &repo, &["add", "-A"]);
            setup(stock, &repo, &["commit", "-q", "-m", &format!("c{i}")]);
            setup(stock, &repo, &["repack", "-q"]);
        }
        setup(stock, &repo, &["prune-packed"]);
        setup(stock, &repo, &["multi-pack-index", "write"]);
        let id = setup(stock, &repo, &["rev-parse", rev]);
        set_midx_offset(stock, &repo, &id, 100_000);
        let (code, _, _) = same(stock, &repo, &["fsck"]);
        match name {
            // The scan parsed the commit; the walk follows that parse and reads nothing.
            "c0" | "c1" => assert_eq!(code, 1, "{name}"),
            // The cache-tree's `parse_object()` reads the index's tree.
            "tree" => assert_eq!(code, 128, "{name}"),
            _ => {}
        }
        same(stock, &repo, &["fsck", "--verbose"]);
        same(stock, &repo, &["fsck", "--no-full"]);
        same(stock, &repo, &["fsck", "--no-full", "--verbose"]);
        same(stock, &repo, &["fsck", "--connectivity-only"]);
        same(stock, &repo, &["fsck", "--root", "--tags", "--unreachable", "--name-objects"]);
    }
    let _ = std::fs::remove_dir_all(&root);
}
