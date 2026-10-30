//! `git fsck`'s own process reads the commit-graph, apart from its `commit-graph verify` child.
//!
//! The first commit `parse_object()` parses goes through `parse_commit_buffer()`'s
//! `load_commit_graph_info()` (commit.c:593-594), so `prepare_commit_graph()` loads the graph and
//! prints the loader's `error()` lines once, ahead of the child's copy; a corrupt generation chunk
//! dies there. A commit nothing parsed before — any non-head under `--connectivity-only` — is
//! parsed by `fsck_walk_commit()`'s `repo_parse_commit()`, which asks the graph first
//! (commit.c:625-640), so the walk follows the graph's parents and tree: a bad edge dies with
//! `invalid parent position`, and an id the graph invents is a broken link.
//!
//! Each fixture is t5318's `full` repository, built with stock git and corrupted at the byte
//! offsets of its `GRAPH_BYTE_*` variables; both binaries then run the same command in it. Stock
//! git is the oracle (`support/stock_git.rs`); the tests skip when it is missing.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-fsck-cg-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir).unwrap()
}

fn cmd(bin: &str, dir: &Path, home: &Path, args: &[&str], env: &[(&str, &str)], date: u64) -> Command {
    let mut c = Command::new(bin);
    c.args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", format!("@{date} +0000"))
        .env("GIT_COMMITTER_DATE", format!("@{date} +0000"))
        .env("GIT_PROGRESS_DELAY", "0")
        .env("LC_ALL", "C");
    c.envs(env.iter().copied());
    c
}

/// Run stock git for fixture setup; it must succeed. Returns stdout.
fn setup(stock: &str, dir: &Path, home: &Path, args: &[&str]) -> String {
    let out = cmd(stock, dir, home, args, &[], 1_700_000_000).output().unwrap();
    assert!(out.status.success(), "setup {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `(code, stdout, stderr)` of one run.
fn run(bin: &str, dir: &Path, home: &Path, args: &[&str], env: &[(&str, &str)]) -> (i32, String, String) {
    let out = cmd(bin, dir, home, args, env, 1_700_000_000).output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Both binaries agree on exit code, stdout and stderr; stock's triple is returned.
#[track_caller]
fn same(stock: &str, dir: &Path, args: &[&str], env: &[(&str, &str)], label: &str) -> (i32, String, String) {
    let theirs = run(stock, dir, dir, args, env);
    let ours = run(ZVCS, dir, dir, args, env);
    assert_eq!(ours, theirs, "{label}: git {args:?} {env:?} (left zvcs, right stock)");
    theirs
}

fn poke(file: &Path, at: u64, bytes: &[u8]) {
    use std::io::{Seek, SeekFrom, Write};
    let mut perms = std::fs::metadata(file).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    std::fs::set_permissions(file, perms).unwrap();
    let mut f = std::fs::OpenOptions::new().write(true).open(file).unwrap();
    f.seek(SeekFrom::Start(at)).unwrap();
    f.write_all(bytes).unwrap();
}

/// The offset of chunk `id` in a graph's table of contents.
fn chunk_offset(graph: &[u8], id: &[u8; 4]) -> u64 {
    let count = graph[6] as usize;
    (0..count)
        .map(|i| &graph[8 + 12 * i..8 + 12 * (i + 1)])
        .find(|entry| &entry[..4] == id)
        .map(|entry| u64::from_be_bytes(entry[4..12].try_into().unwrap()))
        .unwrap()
}

/// t5318's `full` repository at `commits/8` (t5318-commit-graph.sh:446-452): nine commits, one an
/// octopus merge that no ref points at, under a graph of the given generation version.
fn t5318_full(stock: &str, root: &Path, generation_version: &str) -> PathBuf {
    let repo = root.join("full");
    setup(stock, root, root, &["init", "-q", "-b", "main", "full"]);
    let mut date = 1_112_911_993;
    let mut tc = |name: &str| {
        date += 60;
        std::fs::write(repo.join(format!("{name}.t")), name).unwrap();
        setup(stock, &repo, root, &["add", &format!("{name}.t")]);
        let out = cmd(stock, &repo, root, &["commit", "-q", "-m", name], &[], date).output().unwrap();
        assert!(out.status.success());
        setup(stock, &repo, root, &["branch", &format!("commits/{name}")]);
    };
    for i in ["1", "2", "3"] {
        tc(i);
    }
    setup(stock, &repo, root, &["reset", "-q", "--hard", "commits/1"]);
    tc("4");
    tc("5");
    setup(stock, &repo, root, &["reset", "-q", "--hard", "commits/2"]);
    tc("6");
    tc("7");
    setup(stock, &repo, root, &["reset", "-q", "--hard", "commits/3"]);
    setup(stock, &repo, root, &["merge", "-q", "-m", "octopus", "commits/5", "commits/7"]);
    tc("8");
    setup(stock, &repo, root, &["repack", "-q", "-d"]);
    setup(stock, &repo, root, &["config", "core.commitGraph", "true"]);
    let version = format!("commitGraph.generationVersion={generation_version}");
    setup(stock, &repo, root, &["-c", &version, "commit-graph", "write", "--reachable"]);
    repo
}

const HASH_LEN: u64 = 20;
const NUM_COMMITS: u64 = 9;

/// A header the loader rejects is reported by fsck's own `prepare_commit_graph()` and again by
/// the verify child; a tree argument leaves the first commit parse to the object scan.
#[test]
fn loader_errors_come_from_fsck_itself() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("loader");
    let repo = t5318_full(stock, &root, "1");
    let graph = repo.join(".git/objects/info/commit-graph");
    let pristine = std::fs::read(&graph).unwrap();
    let tree = setup(stock, &repo, &root, &["rev-parse", "commits/1^{tree}"]);
    let tree = tree.trim();
    let fanout = 8 + 12 * 5;
    let cases: &[(&str, u64, &[u8])] = &[
        ("signature", 0, b"\0"),
        ("version", 4, b"\x02"),
        ("low chunk count", 6, b"\x01"),
        ("missing lookup", 8 + 12, b"\0"),
        ("fanout order", fanout + 16, b"\x01"),
        ("fanout final", fanout + 4 * 255, b"\x01"),
    ];
    for &(label, at, bytes) in cases {
        poke(&graph, 0, &pristine);
        poke(&graph, at, bytes);
        for args in [
            &["fsck"][..],
            &["fsck", "--progress"],
            &["fsck", "--connectivity-only"],
            &["fsck", "--no-full"],
            &["fsck", tree],
        ] {
            let (code, _, err) = same(stock, &repo, args, &[], label);
            assert_ne!(code & 16, 0, "{label}: {args:?}: ERROR_COMMIT_GRAPH");
            let line = err.lines().find(|l| l.starts_with("error: ")).unwrap();
            assert_eq!(err.matches(line).count(), 2, "{label}: {args:?}: the parent and the child each report");
        }
        // core.commitGraph=false keeps the graph closed in both processes.
        let (code, _, err) = same(stock, &repo, &["-c", "core.commitGraph=false", "fsck"], &[], label);
        assert_eq!((code, err.contains("commit-graph")), (0, false), "{label}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// Without a head parse first, the walk parses commits from the graph: the octopus merge's bad
/// extra edge dies, and a lookup entry rewritten to an id the odb lacks is a broken link.
#[test]
fn connectivity_walk_follows_graph_parents() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("walk");
    let repo = t5318_full(stock, &root, "1");
    let graph = repo.join(".git/objects/info/commit-graph");
    let pristine = std::fs::read(&graph).unwrap();
    let oid_lookup = 8 + 12 * 5 + 4 * 256;
    let commit_data = oid_lookup + HASH_LEN * NUM_COMMITS;

    // GRAPH_BYTE_OCTOPUS: the merge's second extra edge points past the graph.
    poke(&graph, chunk_offset(&pristine, b"EDGE") + 4, b"\x01");
    for (args, env, code) in [
        (&["fsck", "--connectivity-only"][..], &[][..], 128),
        (&["fsck", "--connectivity-only", "commits/8"], &[], 128),
        // `--no-full` never walks into a pack (`mark_object()` stops at `has_object_pack()`).
        (&["fsck", "--no-full"], &[], 18),
        (&["fsck", "--no-full", "--no-reflogs"], &[], 16),
        // A full scan parses every commit from the object database first.
        (&["fsck"], &[], 16),
        (&["-c", "core.commitGraph=false", "fsck", "--connectivity-only"], &[], 0),
        (&["-c", "core.commitGraph=false", "fsck", "--connectivity-only"], &[("GIT_TEST_COMMIT_GRAPH", "1")], 128),
        (&["fsck", "--connectivity-only"], &[("GIT_COMMIT_GRAPH_PARANOIA", "1")], 128),
    ] {
        let (got, _, err) = same(stock, &repo, args, env, "octopus edge");
        assert_eq!(got, code, "octopus edge: {args:?} {env:?}");
        if code == 128 {
            assert!(err.starts_with("fatal: invalid parent position "), "{args:?}: {err}");
        }
    }
    // A shallow file makes commit_graph_compatible() refuse the graph in fsck's own process.
    let tip = setup(stock, &repo, &root, &["rev-parse", "commits/1"]);
    std::fs::write(repo.join(".git/shallow"), &tip).unwrap();
    let (code, _, _) = same(stock, &repo, &["fsck", "--connectivity-only"], &[], "shallow");
    assert_eq!(code, 16);
    std::fs::remove_file(repo.join(".git/shallow")).unwrap();

    // GRAPH_BYTE_OID_LOOKUP_MISSING's corruption, aimed at the octopus merge's last parent: the
    // merge names it by graph position, so the walk follows an id the object database does not
    // have and reports the link broken and the id missing.
    poke(&graph, 0, &pristine);
    let mut lexical: Vec<String> = setup(stock, &repo, &root, &["rev-list", "--all"]).lines().map(str::to_owned).collect();
    lexical.sort();
    let parent = setup(stock, &repo, &root, &["rev-parse", "commits/8^^3"]);
    let index = lexical.iter().position(|oid| oid == parent.trim()).unwrap() as u64;
    poke(&graph, oid_lookup + HASH_LEN * index + 10, b"\x01");
    for args in [
        &["fsck", "--connectivity-only", "--no-dangling"][..],
        &["fsck", "--connectivity-only", "--no-dangling", "--name-objects"],
    ] {
        let (code, out, _) = same(stock, &repo, args, &[], "oid lookup");
        assert_eq!(code, 18, "{args:?}");
        assert!(out.starts_with("broken link from  commit "), "{args:?}: {out}");
    }

    // GRAPH_BYTE_COMMIT_TREE of the first commit in lexical order, which a ref names: its tree
    // came from the object database, so only the verify child notices.
    poke(&graph, 0, &pristine);
    poke(&graph, commit_data, b"\x01");
    let (code, out, _) = same(stock, &repo, &["fsck", "--connectivity-only", "--no-dangling"], &[], "tree");
    assert_eq!((code, out.as_str()), (16, ""));

    // GIT_TEST_COMMIT_GRAPH_DIE_ON_PARSE fires on the first graph parse, which only a walk past
    // the heads reaches.
    poke(&graph, 0, &pristine);
    let die = [("GIT_TEST_COMMIT_GRAPH_DIE_ON_PARSE", "true")];
    assert_eq!(same(stock, &repo, &["fsck", "--connectivity-only"], &die, "die on parse").0, 128);
    assert_eq!(same(stock, &repo, &["fsck"], &die, "die on parse").0, 0);
    let _ = std::fs::remove_dir_all(&root);
}

/// A generation-data offset with the overflow bit set and no GDO2 chunk dies in
/// `fill_commit_graph_info()` at the first head fsck parses.
#[test]
fn generation_overflow_dies_at_the_head_parse() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("overflow");
    let repo = t5318_full(stock, &root, "2");
    let graph = repo.join(".git/objects/info/commit-graph");
    let pristine = std::fs::read(&graph).unwrap();
    let gda2 = chunk_offset(&pristine, b"GDA2");
    for index in [0, 4, 8] {
        poke(&graph, 0, &pristine);
        poke(&graph, gda2 + 4 * index, b"\x80");
        for args in [&["fsck"][..], &["fsck", "--connectivity-only"], &["fsck", "--no-full"]] {
            let (code, _, err) = same(stock, &repo, args, &[], "overflow");
            assert_eq!(code, 128, "{args:?}");
            assert!(err.ends_with("fatal: commit-graph requires overflow generation data but has none\n"), "{err}");
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}
