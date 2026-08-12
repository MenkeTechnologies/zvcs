//! Pack plumbing behaviours that were wrong and are now pinned.
//!
//! Five separate claims, each checked against an observable the harness also
//! reads, and none of them dependent on a system `git` being installed — every
//! fixture is built with the binary under test:
//!
//!   1. `pack-objects` writes entries in `traverse_commit_list()` order: the
//!      commit, then its root tree, then that tree's entries depth-first in tree
//!      order — *not* grouped by type. `compute_write_order()` leaves that order
//!      alone while no object sits at a tag tip.
//!   2. A bare object list on stdin packs exactly the ids it names.
//!      `read_object_list_from_stdin()` calls `add_object_entry()` per line and
//!      no traversal follows, so naming a commit packs the commit alone; an id
//!      naming no object is fatal at the write; a line that is not a hex id is
//!      fatal while the list is read.
//!   3. `verify-pack -v`'s third column is `show_pack_info()`'s `obj->size`, the
//!      size decoded from the entry's own pack header — the *delta stream*
//!      length for a delta entry, not the size of the object it reconstructs.
//!   4. `index-pack --stdin` accepts a pack whose header declares zero objects
//!      and writes the pack, its index and its reverse index like any other.
//!   5. `repack -d` prunes the loose copies of packed objects even when there
//!      was nothing new to pack, and `--write-midx` leaves a `multi-pack-index`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A pack whose header declares zero objects: the twelve-byte header and the
/// SHA-1 trailer over it, nothing between. `index-pack --stdin` has to accept it.
const EMPTY_PACK: &[u8] = b"PACK\x00\x00\x00\x02\x00\x00\x00\x00\
\x02\x9d\x08\x82\x3b\xd8\xa8\xea\xb5\x10\xad\x6a\xc7\x5c\x82\x3c\xfd\x3e\xd3\x1e";

/// Run the binary under test in `dir` under a pinned identity and clock, with
/// stdin taken from `input`. Nothing here reads the ambient environment.
fn run(dir: &Path, home: &Path, args: &[&str], input: &[u8]) -> Output {
    let mut child = Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("TERM", "dumb")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn binary under test");
    {
        use std::io::Write;
        child.stdin.as_mut().expect("stdin").write_all(input).expect("write stdin");
    }
    child.wait_with_output().expect("wait for binary under test")
}

/// `run`, asserting success and returning stdout as a `String`.
fn ok(dir: &Path, home: &Path, args: &[&str]) -> String {
    let out = run(dir, home, args, b"");
    assert!(
        out.status.success(),
        "{args:?} failed ({}): {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// An isolated root plus an empty `HOME`, so no global configuration leaks in.
fn root(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-packorder-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    (root, home)
}

/// One commit holding `README.md` and `src/lib.rs`: the smallest shape whose
/// traversal order and type-grouped order differ, since its root tree names a
/// blob before a subtree.
fn one_commit(tag: &str) -> (PathBuf, PathBuf) {
    let (root, home) = root(tag);
    let repo = root.join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    ok(&repo, &home, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "# fixture\n").unwrap();
    std::fs::write(repo.join("src/lib.rs"), "pub fn one() -> u32 { 1 }\n").unwrap();
    ok(&repo, &home, &["add", "."]);
    ok(&repo, &home, &["commit", "-q", "-m", "initial"]);
    (repo, home)
}

/// Index `pack` in its own directory and return `verify-pack -v`'s object rows,
/// which are in ascending pack-offset order — i.e. the order the pack was
/// written in.
fn pack_rows(dir: &Path, home: &Path, pack: &[u8], tag: &str) -> Vec<Vec<String>> {
    let out = dir.join(format!("unpacked-{tag}"));
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("p.pack"), pack).unwrap();
    ok(&out, home, &["index-pack", "p.pack"]);
    ok(&out, home, &["verify-pack", "-v", "p.idx"])
        .lines()
        .filter(|l| l.len() > 40 && l.as_bytes()[40] == b' ')
        .filter(|l| l[..40].bytes().all(|b| b.is_ascii_hexdigit()))
        .map(|l| l.split_whitespace().map(str::to_string).collect())
        .collect()
}

/// `traverse_commit_list()` shows the commit, then its tree, then the tree's
/// entries in tree order, descending into a subtree as it is met — so the blob
/// `README.md` precedes the `src` tree, which precedes `src/lib.rs`. A writer
/// that grouped by type would emit both trees before either blob.
#[test]
fn write_order_is_traversal_order_not_type_grouped() {
    let (repo, home) = one_commit("order");
    let head = ok(&repo, &home, &["rev-parse", "HEAD"]).trim().to_string();
    let root_tree = ok(&repo, &home, &["rev-parse", "HEAD^{tree}"]).trim().to_string();
    let readme = ok(&repo, &home, &["rev-parse", "HEAD:README.md"]).trim().to_string();
    let src_tree = ok(&repo, &home, &["rev-parse", "HEAD:src"]).trim().to_string();
    let lib = ok(&repo, &home, &["rev-parse", "HEAD:src/lib.rs"]).trim().to_string();

    let out = run(&repo, &home, &["pack-objects", "--all", "--revs", "--stdout"], b"");
    assert!(out.status.success(), "pack-objects failed: {}", String::from_utf8_lossy(&out.stderr));

    let rows = pack_rows(&repo, &home, &out.stdout, "order");
    let order: Vec<&str> = rows.iter().map(|r| r[0].as_str()).collect();
    assert_eq!(order, vec![head, root_tree, readme, src_tree, lib], "pack write order");
}

/// `add_object_entry()` adds the one object the line named. With no internal rev
/// list there is no traversal, so naming a commit packs the commit and leaves
/// its tree out entirely.
#[test]
fn object_list_on_stdin_packs_only_the_named_objects() {
    let (repo, home) = one_commit("stdin-list");
    let head = ok(&repo, &home, &["rev-parse", "HEAD"]).trim().to_string();

    let out = run(&repo, &home, &["pack-objects", "--stdout"], format!("{head}\n").as_bytes());
    assert!(out.status.success(), "pack-objects failed: {}", String::from_utf8_lossy(&out.stderr));

    let rows = pack_rows(&repo, &home, &out.stdout, "stdin-list");
    assert_eq!(rows.len(), 1, "one line in, one object packed: {rows:?}");
    assert_eq!(rows[0][0], head);
    assert_eq!(rows[0][1], "commit");
}

/// An id that resolves to no object is not checked while the list is read; it
/// becomes an entry and `write_no_reuse_object()` dies on it. Nothing reaches
/// stdout, git's pack bytes still being in the hashfile buffer when it dies.
#[test]
fn object_list_on_stdin_dies_on_an_unreadable_id() {
    let (repo, home) = one_commit("stdin-missing");
    let zeros = "0".repeat(40);

    let out = run(&repo, &home, &["pack-objects", "--stdout"], format!("{zeros}\n").as_bytes());
    assert_eq!(out.status.code(), Some(128), "exit code");
    assert!(out.stdout.is_empty(), "stdout: {:?}", String::from_utf8_lossy(&out.stdout));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr).trim_end(),
        format!("fatal: unable to read {zeros}")
    );
}

/// `parse_oid_hex()` accepts nothing but a hex id, and the line is echoed back
/// on the second line of the diagnostic — with its own newline, on top of the
/// one `vreportf()` appends unconditionally. A rev is not a hex id.
#[test]
fn object_list_on_stdin_rejects_a_line_that_is_not_a_hex_id() {
    let (repo, home) = one_commit("stdin-garbage");

    let out = run(&repo, &home, &["pack-objects", "--stdout"], b"HEAD\n");
    assert_eq!(out.status.code(), Some(128), "exit code");
    assert!(out.stdout.is_empty(), "stdout: {:?}", String::from_utf8_lossy(&out.stdout));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "fatal: expected object ID, got garbage:\n HEAD\n\n"
    );
}

/// `show_pack_info()` prints `obj->size`, decoded from the entry's own pack
/// header. For a delta that is the length of the delta instruction stream, which
/// is far smaller than the object it reconstructs — the whole point of storing
/// the delta. Printing the reconstructed size instead is the bug this pins.
#[test]
fn verify_pack_reports_the_delta_stream_size_for_a_delta_entry() {
    let (root, home) = root("delta-size");
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    ok(&repo, &home, &["init", "-q", "-b", "main"]);

    // Two revisions of one 400-line file: the second is a near-copy of the
    // first, which is what gives the delta search something to find.
    let body = |edited: bool| -> String {
        (1..=400)
            .map(|i| {
                if edited && i == 200 {
                    format!("payload line {i} edited\n")
                } else {
                    format!("payload line {i}\n")
                }
            })
            .collect()
    };
    std::fs::write(repo.join("big.txt"), body(false)).unwrap();
    ok(&repo, &home, &["add", "big.txt"]);
    ok(&repo, &home, &["commit", "-q", "-m", "r0"]);
    std::fs::write(repo.join("big.txt"), body(true)).unwrap();
    ok(&repo, &home, &["commit", "-qam", "r1"]);

    let out = run(&repo, &home, &["pack-objects", "--all", "--revs", "--stdout"], b"");
    assert!(out.status.success(), "pack-objects failed: {}", String::from_utf8_lossy(&out.stderr));
    let rows = pack_rows(&repo, &home, &out.stdout, "delta-size");

    // A delta row carries two extra columns: the chain depth and the base id.
    let delta = rows
        .iter()
        .find(|r| r.len() == 7)
        .unwrap_or_else(|| panic!("no delta entry in the pack: {rows:?}"));
    let reported: u64 = delta[2].parse().expect("size column");
    let real: u64 = ok(&repo, &home, &["cat-file", "-s", &delta[0]]).trim().parse().unwrap();
    assert!(
        reported < real,
        "delta entry {} reported size {reported}, object is {real} bytes: the size column must be \
         the delta stream length, not the reconstructed object size",
        delta[0]
    );
}

/// A header declaring zero objects is ordinary input: the object loop runs zero
/// times and the pack, its index and its reverse index are all written.
#[test]
fn index_pack_stdin_accepts_a_pack_with_no_objects() {
    let (repo, home) = one_commit("empty-pack");

    let out = run(&repo, &home, &["index-pack", "--stdin"], EMPTY_PACK);
    assert!(out.status.success(), "index-pack failed: {}", String::from_utf8_lossy(&out.stderr));
    let hash = "029d08823bd8a8eab510ad6ac75c823cfd3ed31e";
    assert_eq!(String::from_utf8_lossy(&out.stdout), format!("pack\t{hash}\n"));

    let dir = repo.join(".git/objects/pack");
    for ext in ["pack", "idx", "rev"] {
        let path = dir.join(format!("pack-{hash}.{ext}"));
        assert!(path.is_file(), "{} not written", path.display());
    }
    // The temporary git streams stdin into becomes the pack through a rename, so
    // none may be left behind.
    let leftovers: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("tmp_pack_"))
        .collect();
    assert!(leftovers.is_empty(), "temporaries left behind: {leftovers:?}");
}

/// `--strict` runs `fsck_object()` over every object plus `check_objects()`'s
/// link checks. A pack built from a healthy repository passes them all and is
/// indexed exactly as it would be without the flag.
#[test]
fn index_pack_strict_indexes_a_healthy_pack() {
    let (repo, home) = one_commit("strict");
    let out = run(&repo, &home, &["pack-objects", "--all", "--revs", "--stdout"], b"");
    assert!(out.status.success(), "pack-objects failed: {}", String::from_utf8_lossy(&out.stderr));

    let dir = repo.join("packs");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("p.pack"), &out.stdout).unwrap();

    let strict = run(&dir, &home, &["index-pack", "--strict", "p.pack"], b"");
    assert!(
        strict.status.success(),
        "index-pack --strict failed: {}",
        String::from_utf8_lossy(&strict.stderr)
    );
    assert!(dir.join("p.idx").is_file(), "index not written under --strict");

    // The hash it reports is the pack's own checksum, the same one a plain run
    // prints — `--strict` adds checks, it does not change the index.
    let plain = ok(&dir, &home, &["index-pack", "-o", "plain.idx", "p.pack"]);
    assert_eq!(String::from_utf8_lossy(&strict.stdout), plain);
}

/// `if (!names.nr)` only reports; the rest of `cmd_repack()` still runs. So a
/// `-d` that had nothing new to pack still reaches `prune_packed_objects()` and
/// drops the loose copies of objects an existing pack already holds.
#[test]
fn repack_d_prunes_loose_objects_with_nothing_new_to_pack() {
    let (repo, home) = one_commit("prune-loose");

    let loose = |label: &str| -> usize {
        let dir = repo.join(".git/objects");
        let mut n = 0;
        for entry in std::fs::read_dir(&dir).unwrap().filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.len() == 2 && name.bytes().all(|b| b.is_ascii_hexdigit()) {
                n += std::fs::read_dir(entry.path()).unwrap().filter_map(Result::ok).count();
            }
        }
        assert!(n > 0 || label.is_empty(), "{label}: expected loose objects");
        n
    };

    // A pack without `-d`, so every object is both packed and loose.
    ok(&repo, &home, &["repack", "-a", "-q"]);
    let before = loose("after repack -a");
    assert!(before > 0, "fixture should still have loose objects");

    // Nothing new to pack — everything is in the pack already — but `-d` still
    // prunes what the pack covers.
    ok(&repo, &home, &["repack", "-d", "-q"]);
    assert_eq!(loose(""), 0, "repack -d left {before} loose objects unpruned");
}

/// `repack_write_midx()` runs whenever `--write-midx` is given, so the run
/// leaves a `multi-pack-index` beside the pack it wrote.
#[test]
fn repack_write_midx_leaves_a_multi_pack_index() {
    let (repo, home) = one_commit("midx");
    ok(&repo, &home, &["repack", "-a", "-d", "-q", "--write-midx"]);
    assert!(
        repo.join(".git/objects/pack/multi-pack-index").is_file(),
        "multi-pack-index not written"
    );
}

/// The first layer of a chain has no base graph, so its bytes are the
/// single-file graph's; only the naming and the chain file differ.
#[test]
fn commit_graph_split_writes_a_one_layer_chain() {
    let (repo, home) = one_commit("split");
    ok(&repo, &home, &["commit-graph", "write", "--reachable", "--split"]);

    let dir = repo.join(".git/objects/info/commit-graphs");
    let chain = std::fs::read_to_string(dir.join("commit-graph-chain")).expect("chain file");
    let hash = chain.trim();
    assert_eq!(hash.len(), 40, "chain names one layer: {chain:?}");
    let layer = dir.join(format!("graph-{hash}.graph"));
    let bytes = std::fs::read(&layer).unwrap_or_else(|e| panic!("{}: {e}", layer.display()));

    // The layer is named by its own trailing checksum, and declares no base.
    assert_eq!(&bytes[..4], b"CGPH");
    assert_eq!(bytes[7], 0, "a first layer has no base graph");
    assert_eq!(hex(&bytes[bytes.len() - 20..]), hash, "layer name is its own checksum");

    // A plain write of the same repository produces the same graph.
    let (plain, plain_home) = one_commit("split-plain");
    ok(&plain, &plain_home, &["commit-graph", "write", "--reachable"]);
    let single = std::fs::read(plain.join(".git/objects/info/commit-graph")).unwrap();
    assert_eq!(single, bytes, "a one-layer chain is the single-file graph");
}

/// Lowercase hex, for comparing a trailer against the name it is written under.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
