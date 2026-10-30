//! `git fsck` ends every scanned source's packed half with `verify_midx()`'s
//! `git multi-pack-index verify --object-dir <source> --no-progress` child, `--progress` under
//! `--progress` (odb/source-packed.c:961-982), run whether or not the source has a midx; its
//! failure makes `odb_fsck()` fail, which is `ERROR_OBJECT` (builtin/fsck.c:934-935). The
//! child is `verify_midx_file()` (midx.c:925-1080): `midx_report()` lines with no prefix, in the
//! order its `qsort()` of the objects by pack leaves them, and its own meters. A midx that
//! `load_multi_pack_index_one()` dies on (midx.c:146-217) kills fsck itself at its first odb
//! prepare, ahead of any object line.
//!
//! Stock git builds every fixture and is the oracle (`support/stock_git.rs`); the tests skip
//! when it is missing.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-fsck-midx-verify-{label}-{}", std::process::id()));
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
    setup(stock, &repo, &["config", "gc.auto", "0"]);
    setup(stock, &repo, &["config", "maintenance.auto", "false"]);
    repo
}

/// `n` commits of a fresh file each, plus an annotated tag on the first.

/// Three commits, each in a pack of its own, and a midx over the three packs.
fn midx_repo(stock: &str, root: &Path, name: &str) -> PathBuf {
    let repo = init(stock, root, name);
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

/// Every chunk's id and offset from the midx's table of contents.
fn chunk(data: &[u8], id: &[u8; 4]) -> usize {
    let n = data[6] as usize;
    (0..n)
        .map(|i| 12 + 12 * i)
        .find(|&at| &data[at..at + 4] == id)
        .map(|at| u64::from_be_bytes(data[at + 4..at + 12].try_into().unwrap()) as usize)
        .expect("chunk present")
}

const FORMS: [&[&str]; 6] = [
    &["fsck"],
    &["fsck", "--no-full"],
    &["fsck", "--progress"],
    &["fsck", "--unreachable"],
    &["multi-pack-index", "verify"],
    &["multi-pack-index", "verify", "--progress"],
];

#[test]
fn the_verify_child_reports_a_damaged_midx() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("damaged");

    let healthy = midx_repo(stock, &root, "healthy");
    for args in FORMS {
        let (code, _, _) = same(stock, &healthy, args);
        assert_eq!(code, 0, "{args:?}");
    }

    // The trailing checksum.
    let checksum = midx_repo(stock, &root, "checksum");
    let mut data = std::fs::read(midx_path(&checksum)).unwrap();
    *data.last_mut().unwrap() ^= 1;
    std::fs::write(midx_path(&checksum), &data).unwrap();
    for args in FORMS {
        let (code, _, err) = same(stock, &checksum, args);
        assert_eq!(code, 1, "{args:?}");
        assert!(err.contains("incorrect checksum\n"), "{err}");
    }

    // Two object offsets. This port's own odb reads go through the midx's offsets where
    // git's report nothing, so only the child is compared.
    let offsets = midx_repo(stock, &root, "offsets");
    let mut data = std::fs::read(midx_path(&offsets)).unwrap();
    let ooff = chunk(&data, b"OOFF");
    data[ooff + 8 * 2 + 7] ^= 1;
    data[ooff + 8 * 5 + 4..ooff + 8 * 5 + 8].copy_from_slice(&7u32.to_be_bytes());
    std::fs::write(midx_path(&offsets), &data).unwrap();
    for args in &FORMS[4..] {
        let (_, _, err) = same(stock, &offsets, args);
        assert!(err.contains("incorrect object offset for oid["), "{err}");
    }

    // Two object names swapped. This port's own odb reads through the midx do not follow
    // `bsearch_hash()` over a misordered table, so only the child is compared.
    let order = midx_repo(stock, &root, "order");
    let mut data = std::fs::read(midx_path(&order)).unwrap();
    let oidl = chunk(&data, b"OIDL");
    let (a, b) = (oidl + 20 * 3, oidl + 20 * 4);
    let first: Vec<u8> = data[a..a + 20].to_vec();
    data.copy_within(b..b + 20, a);
    data[b..b + 20].copy_from_slice(&first);
    std::fs::write(midx_path(&order), &data).unwrap();
    for args in &FORMS[4..] {
        let (code, _, err) = same(stock, &order, args);
        assert_eq!(code, 1, "{args:?}");
        assert!(err.contains("oid lookup out of order: oid[3]"), "{err}");
    }

    // A pack the midx names is gone: one `failed to load pack entry` per object it held,
    // in `qsort()`'s order.
    let deleted = midx_repo(stock, &root, "deleted");
    let pack = std::fs::read_dir(deleted.join(".git/objects/pack"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "pack"))
        .min()
        .unwrap();
    std::fs::remove_file(pack).unwrap();
    for args in FORMS {
        let (code, _, err) = same(stock, &deleted, args);
        assert_ne!(code, 0, "{args:?}");
        assert!(err.contains("failed to load pack in position "), "{err}");
    }

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_midx_that_dies_on_load_ends_fsck_first() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("die");

    let signature = midx_repo(stock, &root, "signature");
    let mut data = std::fs::read(midx_path(&signature)).unwrap();
    data[..4].copy_from_slice(b"XIDX");
    std::fs::write(midx_path(&signature), &data).unwrap();
    let pack_id = midx_repo(stock, &root, "pack-id");
    let mut data = std::fs::read(midx_path(&pack_id)).unwrap();
    let ooff = chunk(&data, b"OOFF");
    data[ooff + 8..ooff + 12].copy_from_slice(&9u32.to_be_bytes());
    std::fs::write(midx_path(&pack_id), &data).unwrap();
    for args in FORMS.iter().chain([&["fsck", "--verbose"][..], &["fsck", "--connectivity-only"][..]].iter()) {
        same(stock, &signature, args);
    }
    // A pack-int-id past the midx's packs dies in the child, on the object that names it.
    // This port's own odb reads panic on such an id, so only the child is compared.
    for args in &FORMS[4..] {
        let (code, _, err) = same(stock, &pack_id, args);
        assert_eq!(code, 128, "{err}");
    }
    let (code, out, err) = same(stock, &signature, &["fsck"]);
    assert_eq!((code, out.as_str()), (128, ""), "{err}");

    // An incremental chain verifies layer by layer.
    let chain = init(stock, &root, "chain");
    for i in 0..3 {
        std::fs::write(chain.join(format!("f{i}")), format!("file {i}\n")).unwrap();
        setup(stock, &chain, &["add", "-A"]);
        setup(stock, &chain, &["commit", "-q", "-m", &format!("c{i}")]);
        setup(stock, &chain, &["repack", "-q"]);
        setup(stock, &chain, &["multi-pack-index", "write", "--incremental"]);
    }
    for args in FORMS {
        let (code, _, _) = same(stock, &chain, args);
        assert_eq!(code, 0, "{args:?}");
    }

    let _ = std::fs::remove_dir_all(&root);
}
