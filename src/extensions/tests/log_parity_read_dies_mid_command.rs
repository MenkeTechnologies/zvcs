//! What `log`, `show`, `diff` and `rev-list` have written when an object read dies partway
//! through: exactly what git has written by then, since `exit()` flushes stdio.
//!
//! git reads a commit's objects only when `log_tree_commit()` reaches it, so a read that dies
//! leaves every earlier record printed (log-tree.c:1176-1222). Where in a record it dies decides how
//! much of the record is out: a tree `diff_tree_oid()` cannot read dies before `show_log()`, a blob
//! dies inside `diff_flush()`, after the header, the separator, the formats that read nothing, and
//! the patches of the pairs ahead of it (log-tree.c:939-967, diff.c:4596-4598). `rev-list
//! --objects --in-commit-order` lists each commit's objects before it asks for the next commit, so
//! the walk's reads of later commits never happen (list-objects.c:432-447).
//!
//! Stock git builds every fixture and is the oracle (`support/stock_git.rs`); the tests skip when it
//! is missing. Each zvcs run gets a fresh `ZVCS_HOME`, so no cached tree diff answers for a blob.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-read-dies-{label}-{}", std::process::id()));
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

fn run(bin: &str, dir: &Path, args: &[&str]) -> (i32, String, String) {
    let mut c = cmd(bin, dir, args);
    if bin == ZVCS {
        let home = dir.with_extension("zvcs-home");
        let _ = std::fs::remove_dir_all(&home);
        c.env("ZVCS_HOME", &home);
    }
    let out = c.output().unwrap();
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

/// One commit per entry of `commits`, each adding the files it names, each repacked into a pack
/// of its own and the loose copies pruned.
fn packs_per_commit(stock: &str, root: &Path, name: &str, commits: &[&[&str]]) -> PathBuf {
    setup(stock, root, &["init", "-q", "-b", "main", name]);
    let repo = root.join(name);
    setup(stock, &repo, &["config", "gc.auto", "0"]);
    setup(stock, &repo, &["config", "maintenance.auto", "false"]);
    for (i, files) in commits.iter().enumerate() {
        for file in *files {
            std::fs::write(repo.join(file), format!("{file} in c{i}\n")).unwrap();
        }
        setup(stock, &repo, &["add", "-A"]);
        setup(stock, &repo, &["commit", "-q", "-m", &format!("c{i}")]);
        setup(stock, &repo, &["repack", "-q"]);
    }
    setup(stock, &repo, &["prune-packed"]);
    repo
}

fn three_packs(stock: &str, root: &Path, name: &str) -> PathBuf {
    packs_per_commit(stock, root, name, &[&["f0"], &["f1"], &["f2"]])
}

fn writable(path: &Path) {
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    std::fs::set_permissions(path, perms).unwrap();
}

/// Cut `by` bytes off the end of `pack`, which takes its trailing checksum with them.
fn truncate(pack: &Path, by: u64) {
    writable(pack);
    let file = std::fs::OpenOptions::new().write(true).open(pack).unwrap();
    let len = file.metadata().unwrap().len();
    file.set_len(len - by).unwrap();
}

fn pack_objects(stock: &str, repo: &Path, ids: &str) -> PathBuf {
    let mut child = cmd(stock, repo, &["pack-objects", "-q", ".git/objects/pack/pack"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(ids.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    repo.join(format!(".git/objects/pack/pack-{}.pack", String::from_utf8_lossy(&out.stdout).trim()))
}

/// Everything in one pack but `blob`, which is alone in a second pack cut short.
fn blob_in_truncated_pack(stock: &str, repo: &Path, blob: &str) {
    setup(stock, repo, &["repack", "-adq"]);
    let old: Vec<PathBuf> = std::fs::read_dir(repo.join(".git/objects/pack"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    let alone = pack_objects(stock, repo, &format!("{blob}\n"));
    let rest: String = setup(stock, repo, &["rev-list", "--objects", "--all"])
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|id| *id != blob)
        .map(|id| format!("{id}\n"))
        .collect();
    pack_objects(stock, repo, &rest);
    for path in old {
        writable(&path);
        std::fs::remove_file(path).unwrap();
    }
    truncate(&alone, 5);
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
fn log_p_writes_every_record_ahead_of_a_tree_use_pack_cannot_read() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("tree");
    // Past the end of the pack, and a large-offset reference into a LOFF chunk the
    // multi-pack-index does not have: `use_pack()` dies on both.
    for (name, value) in [("past-end", 100_000u32), ("large-offset", 0x8000_0000)] {
        let repo = three_packs(stock, &root, name);
        setup(stock, &repo, &["multi-pack-index", "write"]);
        let tree = setup(stock, &repo, &["rev-parse", "HEAD~2^{tree}"]);
        set_midx_offset(stock, &repo, &tree, value);
        let (code, out, err) = same(stock, &repo, &["log", "-p", "--format=%s"]);
        assert_eq!((code, err.as_str()), (128, "fatal: offset beyond end of packfile (truncated pack?)\n"));
        // The first record and its patch are out; the second dies in `diff_tree_oid()`, before
        // its header.
        assert!(out.starts_with("c2\n\ndiff --git a/f2 b/f2\n"), "{out}");
        assert!(!out.contains("c1"), "{out}");
        for args in [
            &["log", "-p"][..],
            &["log", "-p", "--stat", "--format=%s"],
            &["log", "--oneline", "-p"],
            &["log", "--summary", "-p", "--format=%s"],
            &["log", "-p", "--line-prefix=> "],
            &["show", "HEAD", "HEAD~1"],
            &["show", "-s", "HEAD~1"],
        ] {
            same(stock, &repo, args);
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_blob_diff_flush_cannot_read_dies_after_the_records_header() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("blob");
    let repo = three_packs(stock, &root, "blob");
    let blob = setup(stock, &repo, &["rev-parse", "HEAD~1:f1"]);
    blob_in_truncated_pack(stock, &repo, &blob);
    let died = format!("fatal: unable to read {blob}\n");
    let (code, out, err) = same(stock, &repo, &["log", "--stat", "--format=%s"]);
    assert_eq!(code, 128);
    assert!(err.ends_with(&died), "{err}");
    assert!(out.ends_with("\nc1\n\n"), "{out}");
    for args in [
        &["log", "-p", "--format=%s"][..],
        &["log", "-p"],
        &["log", "--numstat", "--format=%s"],
        &["log", "--summary", "-p", "--format=%s"],
        &["log", "--raw", "-p", "--format=%s"],
        &["log", "--oneline", "-p"],
        &["log", "-p", "--stat"],
        &["log", "-p", "--line-prefix=> "],
        &["show", "HEAD~1"],
        &["show", "--stat", "HEAD~1"],
        &["show", "--summary", "-p", "HEAD~1"],
        &["show", "HEAD", "HEAD~1"],
        &["diff", "--stat", "HEAD~2", "HEAD"],
        &["diff", "HEAD~2", "HEAD"],
    ] {
        let (code, _, err) = same(stock, &repo, args);
        assert_eq!(code, 128, "{args:?}");
        assert!(err.ends_with(&died), "{args:?}: {err}");
    }
    // Neither format reads a blob, so neither meets the truncated pack.
    for args in [&["show", "--raw", "HEAD~1"][..], &["show", "--name-status", "HEAD~1"], &["log", "--raw", "--format=%s"]] {
        let (code, _, _) = same(stock, &repo, args);
        assert_eq!(code, 0, "{args:?}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_patches_of_the_pairs_ahead_of_an_unreadable_blob_are_written() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("pairs");
    let repo = packs_per_commit(stock, &root, "pairs", &[&["a"], &["a", "b", "c"], &["d"]]);
    let blob = setup(stock, &repo, &["rev-parse", "HEAD~1:b"]);
    blob_in_truncated_pack(stock, &repo, &blob);
    let (code, out, _) = same(stock, &repo, &["log", "-p", "--format=%s"]);
    assert_eq!(code, 128);
    // `a`'s patch is out, `b` dies before its own header.
    assert!(out.ends_with("+a in c1\n"), "{out}");
    for args in [&["show", "HEAD~1"][..], &["show", "--raw", "-p", "HEAD~1"], &["log", "-p", "--stat", "--format=%s"]] {
        same(stock, &repo, args);
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_tree_that_reads_as_a_blob_ends_the_patch_window_where_git_reads_it() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("other");
    let repo = three_packs(stock, &root, "other");
    setup(stock, &repo, &["multi-pack-index", "write"]);
    let tree = setup(stock, &repo, &["rev-parse", "HEAD~2^{tree}"]);
    let blob = setup(stock, &repo, &["rev-parse", "HEAD~2:f0"]);
    // Both live in the first commit's pack: the tree's entry now reads the blob.
    let offset =std::fs::read_dir(repo.join(".git/objects/pack"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "idx"))
        .find_map(|idx| {
            let out = Command::new(stock).arg("show-index").stdin(std::fs::File::open(&idx).unwrap()).output().unwrap();
            String::from_utf8_lossy(&out.stdout).lines().find_map(|l| {
                let mut f = l.split_whitespace();
                let offset = f.next()?;
                (f.next()? == blob).then(|| offset.parse::<u32>().unwrap())
            })
        })
        .expect("blob is packed");
    set_midx_offset(stock, &repo, &tree, offset);
    let (code, _, err) = same(stock, &repo, &["log", "-p", "--format=%s"]);
    assert_eq!((code, err), (128, format!("fatal: unable to read tree ({tree})\n")));
    for args in [&["show", "HEAD~1"][..], &["show", "-s", "HEAD~1"], &["log", "--oneline", "-p"]] {
        same(stock, &repo, args);
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn rev_list_in_commit_order_never_reads_the_commits_after_the_object_it_dies_on() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("in-commit-order");
    let repo = three_packs(stock, &root, "in-commit-order");
    // The first commit's pack: its commit, tree and blob. The newest commit's tree lists
    // that blob, so its listing dies before the walk would read the first commit.
    let tree = setup(stock, &repo, &["rev-parse", "HEAD~2^{tree}"]);
    let pack = std::fs::read_dir(repo.join(".git/objects/pack"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "idx"))
        .find(|idx| {
            let out = Command::new(stock).arg("show-index").stdin(std::fs::File::open(idx).unwrap()).output().unwrap();
            String::from_utf8_lossy(&out.stdout).contains(&tree)
        })
        .expect("tree is packed")
        .with_extension("pack");
    truncate(&pack, 9);
    let (code, out, err) = same(stock, &repo, &["rev-list", "--objects", "--in-commit-order", "--all"]);
    assert_eq!(code, 128);
    assert_eq!(err.matches(" does not match index\n").count(), 2, "{err}");
    assert_eq!(out.lines().count(), 2, "{out}");
    for args in [
        &["rev-list", "--objects", "--in-commit-order", "HEAD"][..],
        &["rev-list", "--objects", "--in-commit-order", "--no-object-names", "--all"],
        &["rev-list", "--objects", "--all"],
    ] {
        same(stock, &repo, args);
    }
    let _ = std::fs::remove_dir_all(&root);
}
