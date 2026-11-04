//! Reads through a pack git will not open, and through a multi-pack-index offset that lands
//! somewhere git cannot use.
//!
//! Before `packfile_fill_entry()` or `midx_fill_entry()` names a pack, `is_pack_valid()` opens it
//! through `open_packed_git_1()`, which checks its size, header, object count and trailing checksum
//! against the `.idx` and reports a pack that fails, such as `packfile %s does not match index` for a
//! truncated one (packfile.c:529-603, :1820-1866, midx.c:618-619). A pack that fails is left closed,
//! so every lookup that lands in it reports it again: once per pass of
//! `do_oid_object_info_extended()`, whose second read every miss gets (odb.c:541-616), and once more
//! on that second read for a multi-pack-index, whose packs `find_pack_entry()` then asks one by one
//! (odb/source-packed.c:48-73). So the count of lines is each command's count of lookups: one
//! `odb_read_object()` per parent in `process_parents()` (revision.c:1189-1194).
//!
//! What a command printed before a read dies stays printed, as `exit()` flushes stdio:
//! `rev-list`'s listing and `log`'s records ahead of `use_pack()`'s `offset beyond end of packfile`.
//!
//! Stock git builds every fixture and is the oracle (`support/stock_git.rs`); the tests skip when it
//! is missing.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-unusable-pack-{label}-{}", std::process::id()));
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

/// Three commits of a file each, each repacked into a pack of its own and the loose copies pruned;
/// `midx` writes a multi-pack-index over the three packs.
fn three_packs(stock: &str, root: &Path, name: &str, midx: bool) -> PathBuf {
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
    setup(stock, &repo, &["prune-packed"]);
    if midx {
        setup(stock, &repo, &["multi-pack-index", "write"]);
    }
    repo
}

/// The `.pack` whose own `.idx` lists `id`, and the offset there.
fn pack_offset(stock: &str, repo: &Path, id: &str) -> (PathBuf, u64) {
    for entry in std::fs::read_dir(repo.join(".git/objects/pack")).unwrap() {
        let idx = entry.unwrap().path();
        if idx.extension().is_none_or(|x| x != "idx") {
            continue;
        }
        let out = Command::new(stock)
            .arg("show-index")
            .stdin(std::fs::File::open(&idx).unwrap())
            .output()
            .unwrap();
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let mut f = line.split_whitespace();
            if let (Some(offset), Some(name)) = (f.next(), f.next()) {
                if name == id {
                    return (idx.with_extension("pack"), offset.parse().unwrap());
                }
            }
        }
    }
    panic!("{id} is in no pack");
}

fn writable(path: &Path) {
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    std::fs::set_permissions(path, perms).unwrap();
}

/// Cut `by` bytes off the end of the pack, which takes its trailing checksum with them.
fn truncate(pack: &Path, by: u64) {
    writable(pack);
    let file = std::fs::OpenOptions::new().write(true).open(pack).unwrap();
    let len = file.metadata().unwrap().len();
    file.set_len(len - by).unwrap();
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
fn a_truncated_pack_is_reported_at_every_lookup_that_lands_in_it() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("truncated");
    for midx in [false, true] {
        let repo = three_packs(stock, &root, &format!("truncated-{midx}"), midx);
        let tree = setup(stock, &repo, &["rev-parse", "HEAD~2^{tree}"]);
        let (pack, _) = pack_offset(stock, &repo, &tree);
        truncate(&pack, 9);
        let (code, _, err) = same(stock, &repo, &["cat-file", "-p", &tree]);
        assert_eq!(code, 128);
        let reports = err.matches(" does not match index\n").count();
        assert_eq!(reports, if midx { 3 } else { 2 }, "{err}");
        for mode in ["-t", "-s", "-e"] {
            same(stock, &repo, &["cat-file", mode, &tree]);
        }
        let (code, out, _) = same(stock, &repo, &["rev-list", "--objects", "--all"]);
        assert_eq!((code, out.lines().count()), (128, 1));
        for args in [
            &["rev-list", "--all"][..],
            &["log", "--stat", "--format=%s"],
            &["log", "--oneline"],
            &["cat-file", "--batch-all-objects", "--batch-check"],
        ] {
            same(stock, &repo, args);
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_output_ahead_of_a_read_that_dies_is_written() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("die");
    // Past the end of the pack, and a large-offset reference into a LOFF chunk the
    // multi-pack-index does not have: `use_pack()` dies on both.
    for (name, value) in [("past-end", 100_000u32), ("large-offset", 0x8000_0000)] {
        let repo = three_packs(stock, &root, name, true);
        let tree = setup(stock, &repo, &["rev-parse", "HEAD~2^{tree}"]);
        set_midx_offset(stock, &repo, &tree, value);
        let (code, out, err) = same(stock, &repo, &["rev-list", "--objects", "--all"]);
        assert_eq!((code, err.as_str()), (128, "fatal: offset beyond end of packfile (truncated pack?)\n"));
        assert!(out.lines().count() > 3, "{out}");
        let (_, out, _) = same(stock, &repo, &["rev-list", "--objects", "--in-commit-order", "--all"]);
        assert!(out.ends_with(&format!("{}\n", setup(stock, &repo, &["rev-parse", "HEAD~2"]))), "{out}");
        let (_, out, _) = same(stock, &repo, &["log", "--stat", "--format=%s"]);
        assert!(out.starts_with("c2\n"), "{out}");
        same(stock, &repo, &["log", "--name-status", "--format=%s"]);
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_multi_pack_index_offset_at_another_object_is_not_a_tree() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("other");
    let repo = three_packs(stock, &root, "other", true);
    let tree = setup(stock, &repo, &["rev-parse", "HEAD~2^{tree}"]);
    let blob = setup(stock, &repo, &["rev-parse", "HEAD~2:f0"]);
    // Both live in the first commit's pack: the tree's entry now reads the blob.
    let (_, offset) = pack_offset(stock, &repo, &blob);
    set_midx_offset(stock, &repo, &tree, offset as u32);
    let (code, _, err) = same(stock, &repo, &["rev-list", "--objects", "--all"]);
    assert_eq!(code, 128);
    assert!(err.starts_with(&format!("error: Object {tree} not a tree\n")), "{err}");
    let (_, _, err) = same(stock, &repo, &["fsck", "--connectivity-only"]);
    assert_eq!(err, format!("error: Object {tree} not a tree\n"));
    let (code, _, err) = same(stock, &repo, &["log", "--stat", "--format=%s"]);
    assert_eq!((code, err), (128, format!("fatal: unable to read tree ({tree})\n")));
    for args in [&["fsck"][..], &["fsck", "--no-full"], &["log", "--raw", "--format=%s"], &["cat-file", "-t", &tree]] {
        same(stock, &repo, args);
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_blob_the_diff_cannot_read_dies_where_diff_populate_filespec_does() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("blob");
    let repo = three_packs(stock, &root, "blob", false);
    setup(stock, &repo, &["repack", "-adq"]);
    // The second commit's blob alone in a pack, everything else in another.
    let blob = setup(stock, &repo, &["rev-parse", "HEAD~1:f1"]);
    let (old, _) = pack_offset(stock, &repo, &blob);
    let mut child = cmd(stock, &repo, &["pack-objects", "-q", ".git/objects/pack/pack"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    writeln!(child.stdin.take().unwrap(), "{blob}").unwrap();
    let alone = String::from_utf8(child.wait_with_output().unwrap().stdout).unwrap();
    let rest = setup(stock, &repo, &["rev-list", "--objects", "--all"]);
    let ids: String = rest
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|id| *id != blob)
        .map(|id| format!("{id}\n"))
        .collect();
    let mut child = cmd(stock, &repo, &["pack-objects", "-q", ".git/objects/pack/pack"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(ids.as_bytes()).unwrap();
    assert!(child.wait().unwrap().success());
    for ext in ["pack", "idx", "rev"] {
        let _ = std::fs::remove_file(old.with_extension(ext));
    }
    truncate(&repo.join(format!(".git/objects/pack/pack-{}.pack", alone.trim())), 5);
    // `show_log()` has printed the record's header by the time the diffstat reads the blob, which
    // this port renders only once the diff is done, so stdout is left out of the comparison.
    for args in [&["log", "--stat", "--format=%s"][..], &["log", "--numstat", "--format=%s"], &["show", "--stat", "HEAD~1"]] {
        let theirs = run(stock, &repo, args);
        let ours = run(ZVCS, &repo, args);
        assert_eq!((ours.0, &ours.2), (theirs.0, &theirs.2), "git {args:?}");
        assert!(theirs.2.ends_with(&format!("fatal: unable to read {blob}\n")), "{}", theirs.2);
    }
    same(stock, &repo, &["log", "--name-status", "--format=%s"]);
    let _ = std::fs::remove_dir_all(&root);
}
