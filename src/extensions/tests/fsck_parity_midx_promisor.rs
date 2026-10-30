//! `git fsck`'s dangling, unreachable and `--verbose` `Checking` lines come out in `obj_hash`
//! slot order, which follows from the order the packs are listed and read in. These fixtures
//! cover the two pack-list shapes that order depends on beyond a plain directory of packs.
//!
//! A `multi-pack-index`: `prepare_pack()` skips every pack the midx names (odb/source-packed.c:
//! 759-761), a read the midx answers loads its owner pack onto the tail of the list without ever
//! moving it (`midx_fill_entry()`, midx.c:592-631, odb/source-packed.c:31-45), and
//! `packfile_store_get_packs()` appends the rest in pack-int-id order (packfile.c:856-866), so
//! the list is the uncovered packs sorted, then the covered packs the snapshot's reads loaded,
//! re-sorted by `odb_reprepare()`, then the remainder. An incremental chain numbers its packs
//! base layer first (midx.c:280-306).
//!
//! The promisor pass: `add_promisor_object()` (packfile.c:2006-2059) parses every object of a
//! promisor pack, asking the commit-graph first (object.c:339-343), and its reads move packs of
//! every source but the one being listed (odb/source-packed.c:456-485), which reorders what
//! `mark_unreachable_referents()` lists later when the promisor pack sits in an alternate.
//!
//! Stock git builds every fixture and is the oracle (`support/stock_git.rs`); the tests skip
//! when it is missing.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-fsck-midx-{label}-{}", std::process::id()));
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
        .stdin(std::process::Stdio::piped());
    c
}

/// Run stock git for fixture setup, feeding `input`; it must succeed. Returns stdout.
fn setup_in(stock: &str, dir: &Path, args: &[&str], input: &str) -> String {
    use std::io::Write;
    let mut child = cmd(stock, dir, args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "setup {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn setup(stock: &str, dir: &Path, args: &[&str]) -> String {
    setup_in(stock, dir, args, "")
}

/// `(code, stdout, stderr)` of one run.
fn run(bin: &str, dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = cmd(bin, dir, args).output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Both binaries agree on exit code, stdout and stderr; stock's triple is returned.
#[track_caller]
fn same(stock: &str, dir: &Path, args: &[&str]) -> (i32, String, String) {
    let theirs = run(stock, dir, args);
    let ours = run(ZVCS, dir, args);
    assert_eq!(ours, theirs, "git {args:?} (left zvcs, right stock)");
    theirs
}

fn init(stock: &str, root: &Path, name: &str) -> PathBuf {
    setup(stock, root, &["init", "-q", "-b", "main", name]);
    let repo = root.join(name);
    // No automatic maintenance: it would repack and rewrite the midx behind the fixture's back.
    setup(stock, &repo, &["config", "gc.auto", "0"]);
    setup(stock, &repo, &["config", "maintenance.auto", "false"]);
    setup(stock, &repo, &["config", "core.commitGraph", "false"]);
    repo
}

/// `rounds` rounds of commits, each ending in its own pack that also holds a fresh set of
/// dangling blobs, a dangling tree and a dangling commit; a few dangling blobs stay loose.
fn packed_rounds(stock: &str, repo: &Path, rounds: usize, files: usize) {
    for r in 0..rounds {
        for i in 0..files + r * 7 {
            let dir = repo.join(format!("d{}", i % 4));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(format!("f{r}-{i}")), format!("f {r} {i}\n")).unwrap();
        }
        setup(stock, repo, &["add", "-A"]);
        setup(stock, repo, &["commit", "-q", "-m", &format!("r{r}")]);
        let mut ids = setup(stock, repo, &["rev-list", "--objects", "--no-object-names", "--all"]);
        for j in 0..6 + r {
            ids.push('\n');
            ids += &setup_in(stock, repo, &["hash-object", "-w", "--stdin"], &format!("dangle {r} {j}\n"));
        }
        let blob = setup_in(stock, repo, &["hash-object", "-w", "--stdin"], &format!("dt {r}\n"));
        let tree = setup_in(stock, repo, &["mktree"], &format!("100644 blob {blob}\tx\n"));
        let commit = setup_in(stock, repo, &["commit-tree", &tree], &format!("dc {r}\n"));
        ids = format!("{ids}\n{blob}\n{tree}\n{commit}\n");
        setup_in(stock, repo, &["pack-objects", "-q", "--incremental", ".git/objects/pack/pack"], &ids);
        setup(stock, repo, &["prune-packed"]);
    }
    for j in 0..7 {
        setup_in(stock, repo, &["hash-object", "-w", "--stdin"], &format!("loose {j}\n"));
    }
}

/// Set every `.pack`'s mtime: all equal when `tie`, otherwise distinct and out of name order.
fn pack_mtimes(repo: &Path, tie: bool) {
    let dir = repo.join(".git/objects/pack");
    let mut packs: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "pack"))
        .collect();
    packs.sort();
    for (k, pack) in packs.iter().enumerate() {
        let secs = if tie { 0 } else { (k as u64 * 37) % 200 };
        let when = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + secs);
        // Packs are written read-only; `futimens()` on a read-only descriptor still sets them.
        std::fs::File::open(pack).unwrap().set_modified(when).unwrap();
    }
}

/// A commit packed after the midx was written, which the midx does not cover.
fn uncovered_pack(stock: &str, repo: &Path) {
    std::fs::write(repo.join("extra"), "extra\n").unwrap();
    setup(stock, repo, &["add", "extra"]);
    setup(stock, repo, &["commit", "-q", "-m", "extra"]);
    let mut ids = setup(stock, repo, &["rev-list", "--objects", "--no-object-names", "HEAD^..HEAD"]);
    ids += "\n";
    ids += &setup_in(stock, repo, &["hash-object", "-w", "--stdin"], "extra dangling\n");
    setup_in(stock, repo, &["pack-objects", "-q", ".git/objects/pack/pack"], &format!("{ids}\n"));
    setup(stock, repo, &["prune-packed"]);
}

const FORMS: [&[&str]; 8] = [
    &["fsck"],
    &["fsck", "--unreachable"],
    &["fsck", "--verbose"],
    &["fsck", "--connectivity-only"],
    &["fsck", "--connectivity-only", "--unreachable"],
    &["fsck", "--connectivity-only", "--verbose"],
    &["fsck", "--connectivity-only", "--no-reflogs", "--unreachable"],
    &["-c", "core.multiPackIndex=false", "fsck", "--connectivity-only", "--unreachable"],
];

#[track_caller]
fn all_forms(stock: &str, repo: &Path) {
    for args in FORMS {
        let (code, out, _) = same(stock, repo, args);
        assert_eq!(code, 0, "{args:?}");
        assert!(out.contains("dangling") || out.contains("unreachable"), "{args:?}: {out}");
    }
}

/// A midx over two, three and four packs, with and without a bitmap, with pack mtimes distinct
/// or tied, and with a pack written after the midx that it does not cover.
#[test]
fn report_order_over_a_multi_pack_index() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("midx");
    for (n, (rounds, bitmap, tie, extra)) in
        [(2, false, false, false), (3, false, false, true), (4, true, false, false), (3, true, true, true)]
            .into_iter()
            .enumerate()
    {
        let repo = init(stock, &root, &format!("r{n}"));
        packed_rounds(stock, &repo, rounds, 30 + n * 20);
        pack_mtimes(&repo, tie);
        match bitmap {
            true => setup(stock, &repo, &["multi-pack-index", "write", "--bitmap"]),
            false => setup(stock, &repo, &["multi-pack-index", "write"]),
        };
        if extra {
            uncovered_pack(stock, &repo);
        }
        all_forms(stock, &repo);
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// The midx and cruft pack `repack --cruft --write-midx` leaves, beside a later pack, and an
/// incremental chain of two layers, whose base layer's packs come first.
#[test]
fn report_order_over_a_cruft_midx_and_an_incremental_chain() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("chain");
    let repo = init(stock, &root, "cruft");
    packed_rounds(stock, &repo, 3, 40);
    setup(stock, &repo, &["repack", "-q", "-a", "-d", "--cruft", "--write-midx"]);
    uncovered_pack(stock, &repo);
    all_forms(stock, &repo);

    let repo = init(stock, &root, "chain");
    packed_rounds(stock, &repo, 3, 50);
    pack_mtimes(&repo, false);
    setup(stock, &repo, &["multi-pack-index", "write", "--incremental"]);
    uncovered_pack(stock, &repo);
    setup(stock, &repo, &["multi-pack-index", "write", "--incremental"]);
    let chain = std::fs::read_to_string(repo.join(".git/objects/pack/multi-pack-index.d/multi-pack-index-chain")).unwrap();
    assert_eq!(chain.lines().count(), 2, "{chain}");
    all_forms(stock, &repo);
    let _ = std::fs::remove_dir_all(&root);
}

/// A server with merges for the commit-graph to hold several parents.
fn server(stock: &str, root: &Path, commits: usize) -> PathBuf {
    let repo = init(stock, root, "server");
    setup(stock, &repo, &["config", "uploadpack.allowFilter", "true"]);
    for c in 0..commits {
        for j in 0..5 {
            let dir = repo.join(format!("s{}", j % 3));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(format!("f{j}")), format!("srv {c} {j}\n")).unwrap();
        }
        setup(stock, &repo, &["add", "-A"]);
        setup(stock, &repo, &["commit", "-q", "-m", &format!("s{c}")]);
        if c % 3 == 2 {
            setup(stock, &repo, &["checkout", "-q", "-b", &format!("b{c}"), "HEAD~1"]);
            std::fs::write(repo.join(format!("b{c}")), "b\n").unwrap();
            setup(stock, &repo, &["add", "-A"]);
            setup(stock, &repo, &["commit", "-q", "-m", &format!("b{c}")]);
            setup(stock, &repo, &["checkout", "-q", "main"]);
            setup(stock, &repo, &["merge", "-q", "--no-edit", &format!("b{c}")]);
        }
    }
    setup(stock, &repo, &["repack", "-q", "-a", "-d"]);
    repo
}

const PROMISOR_FORMS: [&[&str]; 5] = [
    &["fsck", "--connectivity-only"],
    &["fsck", "--connectivity-only", "--unreachable"],
    &["fsck", "--connectivity-only", "--verbose"],
    &["fsck", "--connectivity-only", "--no-reflogs", "--unreachable"],
    &["-c", "core.commitGraph=false", "fsck", "--connectivity-only", "--unreachable"],
];

/// Partial clones under `blob:none` and `tree:0` with a commit-graph written: the promisor
/// pass fills the graph's commits in, parents before the tree, without reading them.
#[test]
fn report_order_in_a_partial_clone_with_a_commit_graph() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("graph");
    let server = server(stock, &root, 45);
    let url = format!("file://{}", server.display());
    for filter in ["blob:none", "tree:0"] {
        let name = format!("clone-{}", &filter[..4]);
        setup(stock, &root, &["clone", "-q", "--no-checkout", &format!("--filter={filter}"), &url, &name]);
        let repo = root.join(&name);
        setup(stock, &repo, &["config", "gc.auto", "0"]);
        setup(stock, &repo, &["config", "maintenance.auto", "false"]);
        setup(stock, &repo, &["commit-graph", "write", "--reachable"]);
        for j in 0..6 {
            setup_in(stock, &repo, &["hash-object", "-w", "--stdin"], &format!("pd {j}\n"));
        }
        for args in PROMISOR_FORMS {
            same(stock, &repo, args);
        }
        let (_, out, _) = same(stock, &repo, &["fsck", "--connectivity-only", "--unreachable"]);
        assert!(out.contains("unreachable"), "{out}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// A partial clone whose promisor pack sits in an alternate, while the repository's own packs
/// hold copies of some of its objects: every promised object the pass reads is found in the
/// repository's own packs first, which move, so the referents pass lists them in that order.
/// Then the same under a midx over the repository's packs.
#[test]
fn report_order_with_the_promisor_pack_in_an_alternate() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("alternate");
    let server = server(stock, &root, 40);
    let url = format!("file://{}", server.display());
    setup(stock, &root, &["clone", "-q", "--no-checkout", "--filter=blob:none", &url, "base"]);
    let base = root.join("base");
    for c in 0..5 {
        std::fs::write(server.join(format!("late{c}")), format!("late {c}\n")).unwrap();
        setup(stock, &server, &["add", "-A"]);
        setup(stock, &server, &["commit", "-q", "-m", &format!("late{c}")]);
    }
    let reference = base.display().to_string();
    setup(stock, &root, &["clone", "-q", "--no-checkout", "--reference", &reference, "--filter=blob:none", &url, "repo"]);
    let repo = root.join("repo");
    setup(stock, &repo, &["config", "gc.auto", "0"]);
    setup(stock, &repo, &["config", "maintenance.auto", "false"]);
    // Two packs of copies of the alternate's commits and trees, with distinct mtimes.
    for (rev, take) in [("HEAD~3", 0..40), ("HEAD~8", 20..60)] {
        let ids = setup(stock, &repo, &["rev-list", "--objects", "--no-object-names", "--missing=allow-promisor", rev]);
        let ids: Vec<&str> = ids.lines().collect();
        let picked = ids[take.start.min(ids.len())..take.end.min(ids.len())].join("\n");
        setup_in(stock, &repo, &["pack-objects", "-q", ".git/objects/pack/pack"], &format!("{picked}\n"));
    }
    pack_mtimes(&repo, false);
    for j in 0..3 {
        setup_in(stock, &repo, &["hash-object", "-w", "--stdin"], &format!("bl {j}\n"));
    }
    for args in PROMISOR_FORMS {
        same(stock, &repo, args);
    }
    setup(stock, &repo, &["multi-pack-index", "write"]);
    for args in PROMISOR_FORMS {
        same(stock, &repo, args);
    }
    let (_, out, _) = same(stock, &repo, &["fsck", "--connectivity-only", "--unreachable"]);
    assert!(out.contains("unreachable"), "{out}");
    let _ = std::fs::remove_dir_all(&root);
}
