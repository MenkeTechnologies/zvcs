//! `commit-graph verify` against corrupt graphs, as `verify_one_commit_graph()` reports them.
//!
//! zvcs used to run gix's integrity walk, which agreed with git only on a valid graph. The port
//! of `graph_verify()` / `verify_commit_graph()` (builtin/commit-graph.c:65-141,
//! commit-graph.c:2745-2948) must match stock byte for byte on stderr and exit code for every
//! corruption t5318's `corrupt_graph_and_verify` makes and for t5324's split-chain ones, with and
//! without the progress meter, since the meter's redraws interleave with the reports and stop
//! advancing once the first loop finds an error.
//!
//! Each fixture is built with stock git and corrupted at a byte offset; both binaries then run
//! the same read-only command in the same directory. Stock git is the oracle
//! (`support/stock_git.rs`); the tests skip when it is missing.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-cg-verify-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir).unwrap()
}

fn cmd(bin: &str, dir: &Path, home: &Path, args: &[&str], date: u64) -> Command {
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
    c
}

/// Run stock git for fixture setup; it must succeed.
fn setup(stock: &str, dir: &Path, home: &Path, args: &[&str]) {
    let out = cmd(stock, dir, home, args, 1_700_000_000).output().unwrap();
    assert!(out.status.success(), "setup {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// `(code, stdout, stderr)` of one run.
fn run(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> (i32, String, String) {
    let out = cmd(bin, dir, home, args, 1_700_000_000).output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[track_caller]
fn same(stock: &str, dir: &Path, home: &Path, args: &[&str], label: &str) -> (i32, String) {
    let theirs = run(stock, dir, home, args);
    let ours = run(ZVCS, dir, home, args);
    assert_eq!(ours, theirs, "{label}: git {args:?} (left zvcs, right stock)");
    (theirs.0, theirs.2)
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

/// A commit touching its own file at a fixed date.
fn commit(stock: &str, repo: &Path, home: &Path, name: &str, date: u64) {
    std::fs::write(repo.join(format!("{name}.t")), name).unwrap();
    setup(stock, repo, home, &["add", &format!("{name}.t")]);
    let out = cmd(stock, repo, home, &["commit", "-q", "-m", name], date).output().unwrap();
    assert!(out.status.success());
}

/// t5318's `full` repository at `commits/8`: nine commits, one of them an octopus merge, under a
/// generation-version-1 graph (t5318-commit-graph.sh:446-452).
fn t5318_full(stock: &str, root: &Path) -> PathBuf {
    let repo = root.join("full");
    setup(stock, root, root, &["init", "-q", "-b", "main", "full"]);
    let mut date = 1_112_911_993;
    let mut tc = |name: &str| {
        date += 60;
        commit(stock, &repo, root, name, date);
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
    setup(stock, &repo, root, &["repack", "-q"]);
    let tip = run(stock, &repo, root, &["rev-parse", "commits/8"]).1;
    let mut write = cmd(stock, &repo, root, &["-c", "commitGraph.generationVersion=1", "commit-graph", "write", "--stdin-commits"], 0);
    write.stdin(std::process::Stdio::piped());
    let mut child = write.spawn().unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(tip.as_bytes()).unwrap();
    assert!(child.wait().unwrap().success());
    repo
}

const MODES: [&[&str]; 3] = [
    &["commit-graph", "verify"],
    &["commit-graph", "verify", "--progress"],
    &["commit-graph", "verify", "--shallow", "--progress"],
];

/// Every corruption of t5318-commit-graph.sh:540-651, at the offsets its `GRAPH_BYTE_*` variables
/// compute for nine commits, two octopus edges and five chunk-table rows.
#[test]
fn t5318_corruptions_report_as_stock() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("t5318");
    let repo = t5318_full(stock, &root);
    let graph = repo.join(".git/objects/info/commit-graph");
    let pristine = std::fs::read(&graph).unwrap();

    let (hash_len, num_commits, lookup_w) = (20u64, 9u64, 12u64);
    let fanout = 8 + lookup_w * 5;
    let oid_lookup = fanout + 4 * 256;
    let commit_data = oid_lookup + hash_len * num_commits;
    let data_w = hash_len + 16;
    let octopus = commit_data + data_w * num_commits;
    let footer = octopus + 4 * 2;
    let generation = commit_data + hash_len + 11;
    let cases: &[(&str, u64, &[u8], Option<u64>, i32)] = &[
        ("signature", 0, b"\0", None, 1),
        ("version", 4, b"\x02", None, 1),
        ("hash version", 5, b"\x03", None, 1),
        ("low chunk count", 6, b"\x01", None, 1),
        ("missing fanout", 8, b"\0", None, 1),
        ("missing lookup", 8 + lookup_w, b"\0", None, 1),
        ("missing commit data", 8 + 2 * lookup_w, b"\0", None, 1),
        ("fanout", fanout + 16, b"\x01", None, 1),
        ("fanout final", fanout + 4 * 255, b"\x01", None, 1),
        ("oid order", oid_lookup + hash_len * 8, b"\x01", None, 1),
        ("oid missing", oid_lookup + hash_len * 4 + 10, b"\x01", None, 1),
        ("tree", commit_data, b"\x01", None, 1),
        ("parent int-id", commit_data + hash_len, b"\x01", None, 128),
        ("extra parent", commit_data + hash_len + 4, b"\0", None, 1),
        ("wrong parent", commit_data + hash_len + 3, b"\x01", None, 1),
        ("generation", generation, b"\x38", None, 1),
        ("date", commit_data + hash_len + 12, b"\x01", None, 1),
        ("octopus parent", octopus + 4, b"\x01", None, 128),
        ("checksum", footer, b"\0", None, 2),
        ("chunk count", 6, b"\xff", Some(8), 1),
        ("generation last", generation + (num_commits - 1) * data_w, b"\0\0\0\0", None, 1),
        ("generation zero", generation, b"\0\0\0\0", None, 1),
    ];
    for &(label, at, bytes, zero_from, code) in cases {
        poke(&graph, 0, &pristine);
        poke(&graph, at, bytes);
        if let Some(z) = zero_from {
            poke(&graph, z, &vec![0; pristine.len() - z as usize]);
        }
        for args in MODES {
            let (got, _) = same(stock, &repo, &root, args, label);
            assert_eq!(got, code, "{label}: stock's exit code moved");
        }
    }
    poke(&graph, 0, &pristine);
    std::fs::write(&graph, "a small graph\n").unwrap();
    let (_, err) = same(stock, &repo, &root, MODES[0], "too small");
    assert_eq!(err, "error: commit-graph file is too small\n");
    let _ = std::fs::remove_dir_all(&root);
}

/// A three-layer chain: the tip is verified first, an error there stops every base layer after
/// its first loop (`verify_commit_graph_error` is never reset), `--shallow` stops at the tip, and
/// the chain file's own damage is reported by `load_commit_graph_chain_fd_st()`.
#[test]
fn split_chain_corruptions_report_as_stock() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("chain");
    let repo = root.join("r");
    setup(stock, &root, &root, &["init", "-q", "-b", "main", "r"]);
    let mut date = 1_112_911_993;
    for layer in [["1", "2", "3"], ["4", "5", "6"], ["7", "8", "9"]] {
        for name in layer {
            date += 100;
            commit(stock, &repo, &root, name, date);
        }
        setup(stock, &repo, &root, &["commit-graph", "write", "--reachable", "--split=no-merge"]);
    }
    let graphs = repo.join(".git/objects/info/commit-graphs");
    let chain_file = graphs.join("commit-graph-chain");
    let chain = std::fs::read_to_string(&chain_file).unwrap();
    let layer = |n: usize| graphs.join(format!("graph-{}.graph", chain.lines().nth(n).unwrap()));
    let saved: Vec<Vec<u8>> = (0..3).map(|n| std::fs::read(layer(n)).unwrap()).collect();
    let restore = || {
        for (n, bytes) in saved.iter().enumerate() {
            poke(&layer(n), 0, bytes);
        }
        poke(&chain_file, 0, chain.as_bytes());
    };

    // Header 8, a chunk table of (chunks + 1) * 12, the 1024-byte fanout, three 20-byte ids:
    // the CDAT chunk of the base (four chunks) and of an upper layer (five, with BASE).
    let base_cdat = 8 + 5 * 12 + 1024 + 60;
    let tip_cdat = 8 + 6 * 12 + 1024 + 60;
    let base_date = base_cdat + 20 + 12 + 3;
    let tip_date = tip_cdat + 20 + 12 + 3;
    let cases: &[(&str, usize, u64, &[u8])] = &[
        ("pristine", 0, 0, b""),
        ("base checksum", 0, saved[0].len() as u64 - 1, b"\0"),
        ("base date", 0, base_date, b"\x01"),
        ("tip date", 2, tip_date, b"\x01"),
        ("tip parent position", 2, tip_cdat + 20, b"\x00\xff\xff\xff"),
        ("tip base chunk", 2, (tip_cdat + 3 * 36 + 12) as u64, b"\x01"),
        ("tip overflow without GDO2", 2, (tip_cdat + 3 * 36) as u64, b"\x80"),
    ];
    for &(label, n, at, bytes) in cases {
        restore();
        poke(&layer(n), at, bytes);
        for args in MODES {
            same(stock, &repo, &root, args, label);
        }
    }
    // Both layers wrong: the tip's report keeps the base from its second loop.
    restore();
    poke(&layer(2), tip_date, b"\x01");
    poke(&layer(0), base_date, b"\x01");
    for args in MODES {
        same(stock, &repo, &root, args, "tip and base date");
    }

    for (label, at, byte) in [("chain base not hex", 30, b"G"), ("chain base unknown", 30, b"A"), ("chain mid not hex", 70, b"G")] {
        restore();
        poke(&chain_file, at, byte);
        for args in MODES {
            same(stock, &repo, &root, args, label);
        }
    }
    restore();
    std::fs::write(&chain_file, "garbage\n").unwrap();
    let (code, err) = same(stock, &repo, &root, MODES[0], "chain too small");
    assert_eq!(code, 128, "{err}");
    restore();
    std::fs::write(layer(1), "garbage\n").unwrap();
    same(stock, &repo, &root, MODES[1], "middle layer garbage");
    let _ = std::fs::remove_dir_all(&root);
}

/// `register_shallow()` empties the parent list of a commit the graph already parsed, and the
/// shallow file is first read by the first object-database parse: a tip-layer commit compares
/// clean, a base-layer one, not parsed yet, is "too long". `disable_replace_refs()` keeps
/// `refs/replace/` out of the comparison.
#[test]
fn shallow_grafts_and_replace_refs_follow_stock() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("graft");
    let repo = root.join("r");
    setup(stock, &root, &root, &["init", "-q", "-b", "main", "r"]);
    let mut date = 1_112_911_993;
    for layer in [["1", "2", "3"], ["4", "5", "6"]] {
        for name in layer {
            date += 100;
            commit(stock, &repo, &root, name, date);
        }
        setup(stock, &repo, &root, &["commit-graph", "write", "--reachable", "--split=no-merge"]);
    }
    let rev = |spec: &str| run(stock, &repo, &root, &["rev-parse", spec]).1;
    let shallow = repo.join(".git/shallow");
    for (label, content) in [("tip commit", rev("HEAD~1")), ("base commit", rev("HEAD~4"))] {
        std::fs::write(&shallow, content).unwrap();
        for args in MODES {
            same(stock, &repo, &root, args, label);
        }
    }
    std::fs::remove_file(&shallow).unwrap();
    setup(stock, &repo, &root, &["replace", "-f", "HEAD~1", "HEAD~3"]);
    let (code, _) = same(stock, &repo, &root, MODES[1], "replaced commit");
    assert_eq!(code, 0);
    let _ = std::fs::remove_dir_all(&root);
}

/// `--object-dir` names any source of the object database, an alternate's alternate included,
/// so the `commit-graph verify` child `fsck` runs per source (builtin/fsck.c:985-999) finds each
/// one, in the pre-order `prepare_alt_odb()` links them.
#[test]
fn transitive_alternates_are_object_dirs_for_verify_and_fsck() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("alt");
    for r in ["a", "b", "c", "d"] {
        setup(stock, &root, &root, &["init", "-q", "-b", "main", r]);
    }
    commit(stock, &root.join("a"), &root, "a", 1_112_911_993);
    setup(stock, &root.join("a"), &root, &["commit-graph", "write", "--reachable"]);
    commit(stock, &root.join("d"), &root, "d", 1_112_912_993);
    setup(stock, &root.join("d"), &root, &["commit-graph", "write", "--reachable", "--split"]);
    let alternates = |repo: &str, lines: &str| {
        std::fs::write(root.join(repo).join(".git/objects/info/alternates"), lines).unwrap()
    };
    alternates("b", &format!("{}\n../../../d/.git/objects\n", root.join("a/.git/objects").display()));
    alternates("c", &format!("{}\n", root.join("b/.git/objects").display()));
    let c = root.join("c");
    commit(stock, &c, &root, "c", 1_112_913_993);
    setup(stock, &c, &root, &["commit-graph", "write", "--reachable"]);

    let a_objects = root.join("a/.git/objects").display().to_string();
    let d_objects = root.join("d/.git/objects").display().to_string();
    let (code, _) = same(stock, &c, &root, &["commit-graph", "verify", "--object-dir", &a_objects], "a");
    assert_eq!(code, 0);
    same(stock, &c, &root, &["commit-graph", "verify", "--object-dir", &d_objects, "--progress"], "d");
    same(stock, &c, &root, &["fsck", "--progress"], "fsck");

    poke(&root.join("a/.git/objects/info/commit-graph"), 1160, b"\x01");
    let (code, _) = same(stock, &c, &root, &["fsck", "--no-progress"], "fsck corrupt alternate");
    assert_eq!(code, 16);
    let _ = std::fs::remove_dir_all(&root);
}
