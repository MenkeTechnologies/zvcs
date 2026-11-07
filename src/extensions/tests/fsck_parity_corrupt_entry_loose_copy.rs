//! `fsck` over a pack entry whose type bits git does not know, with a loose copy of the object
//! left beside the pack.
//!
//! The first read of the entry goes through `packed_object_info()`, whose
//! `packed_to_object_type()` prints `unknown object type 0 at offset <n> in <pack>`
//! (packfile.c:1124-1129, :1380-1385); the entry is then marked bad and the loose copy answers,
//! so every later read is silent and the exit code stays 0. Which read is first depends on where
//! the object is met: a tree the index's cache-tree names is read by `fsck_cache_tree()`'s
//! `parse_object()` (builtin/fsck.c:705-710, object.c:345-346) and kept, so the reachability walk
//! reads it no more; any other tree or commit is read by the walk's `repo_parse_tree()` or
//! `repo_parse_commit()` (fsck.c:361-362, :410).
//!
//! A full `fsck` verifies the pack, which git 2.56 counts as `ERROR_OBJECT` and this port as
//! `ERROR_PACK`, a pending owner decision (see fsck_parity_corrupt_pack.rs), so it compares
//! stdout and stderr only.
//!
//! Stock git builds every fixture and is the oracle (`support/stock_git.rs`); the tests skip
//! when it is missing.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-fsck-corrupt-entry-loose-{label}-{}", std::process::id()));
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

/// Three commits, each adding `d<i>/f`, each repacked into a pack of its own with the loose
/// objects left in place; then the entry of `rev` gets type 0 in its header byte.
fn corrupt_with_loose_copy(stock: &str, root: &Path, name: &str, rev: &str) -> (PathBuf, String) {
    setup(stock, root, &["init", "-q", "-b", "main", name]);
    let repo = root.join(name);
    setup(stock, &repo, &["config", "gc.auto", "0"]);
    setup(stock, &repo, &["config", "maintenance.auto", "false"]);
    for i in 0..3 {
        std::fs::create_dir_all(repo.join(format!("d{i}"))).unwrap();
        std::fs::write(repo.join(format!("d{i}/f")), format!("file {i}\n")).unwrap();
        setup(stock, &repo, &["add", "-A"]);
        setup(stock, &repo, &["commit", "-q", "-m", &format!("c{i}")]);
        setup(stock, &repo, &["repack", "-q"]);
    }
    let id = setup(stock, &repo, &["rev-parse", rev]);
    for entry in std::fs::read_dir(repo.join(".git/objects/pack")).unwrap() {
        let idx = entry.unwrap().path();
        if idx.extension().is_none_or(|x| x != "idx") {
            continue;
        }
        let out = Command::new(stock).arg("show-index").stdin(std::fs::File::open(&idx).unwrap()).output().unwrap();
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let mut f = line.split_whitespace();
            if let (Some(offset), Some(name)) = (f.next(), f.next()) {
                if name == id {
                    let pack = idx.with_extension("pack");
                    let mut perms = std::fs::metadata(&pack).unwrap().permissions();
                    #[allow(clippy::permissions_set_readonly_false)]
                    perms.set_readonly(false);
                    std::fs::set_permissions(&pack, perms).unwrap();
                    let mut data = std::fs::read(&pack).unwrap();
                    let at: usize = offset.parse().unwrap();
                    data[at] &= 0x8f;
                    std::fs::write(&pack, data).unwrap();
                    return (repo, id);
                }
            }
        }
    }
    panic!("{id} is in no pack");
}

#[test]
fn the_first_read_of_the_entry_reports_it_and_the_loose_copy_answers() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("reads");
    // A tree only an old commit reaches (the walk reads it), a subtree and the root tree the
    // cache-tree names (`fsck_cache_tree()` reads them first), and a commit.
    for (name, rev) in [
        ("old-tree", "HEAD~2^{tree}"),
        ("cache-subtree", "HEAD:d1"),
        ("cache-root", "HEAD^{tree}"),
        ("commit", "HEAD~1"),
    ] {
        let (repo, _) = corrupt_with_loose_copy(stock, &root, name, rev);
        let (code, _, err) = same(stock, &repo, &["fsck", "--connectivity-only"]);
        assert_eq!(code, 0);
        assert!(err.starts_with("error: unknown object type 0 at offset "), "{name}: {err}");
        assert_eq!(err.lines().count(), 1, "{name}: {err}");
        same(stock, &repo, &["fsck", "--connectivity-only", "--verbose"]);
        same(stock, &repo, &["fsck", "--connectivity-only", "--unreachable"]);
        same(stock, &repo, &["fsck", "--connectivity-only", "HEAD"]);
        same(stock, &repo, &["fsck", "--no-full"]);
        same(stock, &repo, &["fsck", "--no-full", "--verbose"]);
        let ours = run(ZVCS, &repo, &["fsck"]);
        let theirs = run(stock, &repo, &["fsck"]);
        assert_eq!((ours.1, ours.2), (theirs.1, theirs.2), "{name}: fsck (left zvcs, right stock)");
    }
    let _ = std::fs::remove_dir_all(&root);
}
