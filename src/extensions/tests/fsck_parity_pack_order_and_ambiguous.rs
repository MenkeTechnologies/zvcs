//! `fsck` where the order of the pack list, the order of `obj_hash` and the stdout buffer show,
//! and an `<object>` argument that is an ambiguous abbreviation.
//!
//! A pack `is_pack_valid()` refuses is never walked by `verify_packfile()` (pack-check.c:69-70),
//! so the scan visits the packs on either side of it and nothing of its own; a list whose packs
//! share an mtime keeps the order `readdir()` and the reads before `odb_reprepare()` left it in
//! (packfile.c:1044-1085). `check_reachable_object()`'s `has_object_pack()` walks that list from
//! its head, opening (and reporting) a refused pack that holds the object only when it comes
//! first, and moves the pack that answers to the head (odb/source-packed.c:22-46). The reflog's
//! `lookup_object()` of an object the scan never reached still moves it back to its home slot in
//! `obj_hash` (object.c:92-118), which `check_connectivity()` walks in slot order.
//!
//! `fsck_obj()`'s `root` lines are `printf()`s into stdio's buffer, which the `exit()` of a read
//! that `die()`s afterwards flushes.
//!
//! An abbreviated id that names more than one object is `get_short_oid()`'s `error()` and the
//! `advise()` block listing each candidate (object-name.c:292-401, :507-543), ahead of fsck's
//! own `invalid parameter`; `rev-parse`, `cat-file` and `log` print the same block, a tag's line
//! carrying its `tagger` date.
//!
//! A refused pack makes the full scan's exit code `ERROR_PACK` in this port and `ERROR_OBJECT` in
//! git 2.56, a pending owner decision (see fsck_parity_corrupt_pack.rs), so the scans that verify
//! packs compare stdout and stderr only.
//!
//! Stock git builds every fixture and is the oracle (`support/stock_git.rs`); the tests skip
//! when it is missing.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-fsck-pack-order-{label}-{}", std::process::id()));
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

/// `git <args>` with `input` on stdin, for `pack-objects` and `fast-import`.
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

/// Which `.pack` is younger, which `sort_pack()` puts first; a tie keeps `readdir()` order.
#[derive(Clone, Copy, Debug)]
enum Age {
    Tie,
    BadOlder,
    BadNewer,
}

/// Three commits over files in subdirectories, packed into a good pack and a pack truncated to
/// 200 bytes, which `is_pack_valid()` refuses.
fn refused_pack(stock: &str, root: &Path, shape: Shape, age: Age) -> PathBuf {
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
    repo
}

#[test]
fn a_refused_pack_keeps_its_place_in_the_list_and_its_objects_out_of_the_scan() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("refused");
    for shape in [Shape::Dup, Shape::Tag, Shape::Head] {
        for age in [Age::Tie, Age::BadOlder, Age::BadNewer] {
            let repo = refused_pack(stock, &root, shape, age);
            let (_, err) = same_output(stock, &repo, &["fsck", "--verbose"]);
            assert!(err.contains(" cannot be accessed\n"), "{shape:?} {age:?}: {err}");
            same_output(stock, &repo, &["fsck", "--verbose", "--root", "--unreachable", "--tags", "--name-objects"]);
            // Neither verifies a pack, so the exit codes agree too.
            same(stock, &repo, &["fsck", "--no-full", "--verbose"]);
            same(stock, &repo, &["fsck", "--no-full", "--unreachable", "--verbose"]);
            same(stock, &repo, &["fsck", "--connectivity-only", "--verbose"]);
            same(stock, &repo, &["fsck", "--connectivity-only", "--unreachable", "--verbose"]);
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
fn the_root_lines_ahead_of_a_read_that_dies_are_flushed() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("root-die");
    // Past the end of the pack, and a large-offset reference into a LOFF chunk the
    // multi-pack-index does not have: the walk's read of the oldest tree dies on both.
    for (name, value) in [("past-end", 100_000u32), ("large-offset", 0x8000_0000)] {
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
        let tree = setup(stock, &repo, &["rev-parse", "HEAD~2^{tree}"]);
        set_midx_offset(stock, &repo, &tree, value);
        let first = setup(stock, &repo, &["rev-parse", "HEAD~2"]);
        let (code, out, err) = same(stock, &repo, &["fsck", "--root"]);
        assert_eq!((code, out), (128, format!("root {first}\n")));
        assert!(err.ends_with("fatal: offset beyond end of packfile (truncated pack?)\n"), "{err}");
        same(stock, &repo, &["fsck", "--root", "--verbose"]);
        same(stock, &repo, &["fsck", "--root", "--tags", "--unreachable", "--name-objects"]);
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// A repository holding enough blobs, commits and tags that some four-digit prefixes name
/// two objects; the prefixes, each with the types it names.
fn crowded(stock: &str, root: &Path) -> (PathBuf, BTreeMap<String, Vec<String>>) {
    setup(stock, root, &["init", "-q", "-b", "main", "crowded"]);
    let repo = root.join("crowded");
    let mut stream = String::new();
    for i in 1..=500 {
        let data = format!("blob {i}\n");
        stream.push_str(&format!("blob\ndata {}\n{data}\n", data.len()));
    }
    for i in 1..=300 {
        let msg = format!("m{i}\n");
        stream.push_str(&format!(
            "commit refs/heads/main\ncommitter C O Mitter <committer@example.com> 1112911993 -0700\ndata {}\n{msg}\n",
            msg.len()
        ));
    }
    for i in 1..=200 {
        let msg = format!("msg {i}\n");
        stream.push_str(&format!(
            "tag t{i}\nfrom refs/heads/main\ntagger T <t@example.com> 1112911993 -0700\ndata {}\n{msg}\n",
            msg.len()
        ));
    }
    setup_with_input(stock, &repo, &["fast-import", "--quiet"], stream.as_bytes());
    let listing = setup(stock, &repo, &["cat-file", "--batch-all-objects", "--batch-check=%(objectname) %(objecttype)"]);
    let mut by_prefix: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for line in listing.lines() {
        let (id, kind) = line.split_once(' ').unwrap();
        by_prefix.entry(id[..4].to_owned()).or_default().push(kind.to_owned());
    }
    by_prefix.retain(|_, kinds| kinds.len() > 1);
    (repo, by_prefix)
}

#[test]
fn an_ambiguous_abbreviation_lists_its_candidates() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("ambiguous");
    let (repo, ambiguous) = crowded(stock, &root);
    assert!(ambiguous.values().any(|kinds| kinds.iter().any(|k| k == "tag")), "{ambiguous:?}");
    assert!(ambiguous.values().any(|kinds| kinds.iter().all(|k| k == "blob")), "{ambiguous:?}");
    for (prefix, kinds) in &ambiguous {
        let upper = prefix.to_ascii_uppercase();
        let (code, _, err) = same(stock, &repo, &["fsck", prefix]);
        assert_eq!(code, 1, "{prefix} {kinds:?}");
        assert!(
            err.starts_with(&format!("error: short object ID {prefix} is ambiguous\nhint: The candidates are:\n")),
            "{prefix}: {err}"
        );
        same(stock, &repo, &["fsck", "--verbose", prefix]);
        same(stock, &repo, &["fsck", &upper, "main"]);
        same(stock, &repo, &["fsck", "--connectivity-only", prefix]);
        same(stock, &repo, &["-c", "core.disambiguate=commit", "fsck", prefix]);
        same(stock, &repo, &["rev-parse", prefix]);
        same(stock, &repo, &["cat-file", "-t", prefix]);
        same(stock, &repo, &["log", "--oneline", "-1", prefix]);
    }
    let _ = std::fs::remove_dir_all(&root);
}
