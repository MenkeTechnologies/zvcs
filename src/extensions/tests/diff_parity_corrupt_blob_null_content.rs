//! A packed blob whose zlib data does not inflate, read the way `diff_populate_filespec()` reads
//! it: with `contentp` and `sizep` and no `typep` (diff.c:4580-4617). `packed_object_info()` turns
//! `cache_or_unpack_entry()`'s `NULL` into `-1` only for a caller that asked for the type
//! (packfile.c:1323-1327), so the read succeeds with no content after the unpack's `error()`
//! lines: the stat formats count the side as no lines, and every later populate of it reads it,
//! and prints the lines, again (diff.c:4496-4497). A delta whose own size cannot be read fails the
//! `sizep` read `diff_filespec_is_binary()` makes first (packfile.c:1330-1350), which is
//! `die("unable to read %s")`; `cat-file --batch-check` asks for that size too, and reports the
//! object `missing`.
//!
//! `log -p`, `show` and `diff -p` over such a blob are not compared: stock git 2.56.0 hands xdiff
//! the `NULL` buffer and dies of SIGSEGV (exit 139).
//!
//! `diff-tree` reads the index, so `reuse_worktree_file()` would read a checked-out copy of the
//! blob instead (diff.c:4367-4446); every fixture leaves `f1` modified in the worktree to keep that
//! out of the comparison.
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
    let dir = std::env::temp_dir().join(format!("zvcs-null-content-{label}-{}", std::process::id()));
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

/// `bin` in `dir` with `input` on stdin; zvcs gets `home` as its `ZVCS_HOME`.
fn run_in(bin: &str, dir: &Path, args: &[&str], input: &str, home: &Path) -> (i32, String, String) {
    let mut c = cmd(bin, dir, args);
    if bin == ZVCS {
        c.env("ZVCS_HOME", home);
    }
    let mut child = c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[track_caller]
fn same_with_input(stock: &str, dir: &Path, args: &[&str], input: &str) -> (i32, String, String) {
    let home = dir.with_extension("zvcs-home");
    let _ = std::fs::remove_dir_all(&home);
    let theirs = run_in(stock, dir, args, input, &home);
    let ours = run_in(ZVCS, dir, args, input, &home);
    assert_eq!(ours, theirs, "git {args:?} in {} (left zvcs, right stock)", dir.display());
    theirs
}

#[track_caller]
fn same(stock: &str, dir: &Path, args: &[&str]) -> (i32, String, String) {
    same_with_input(stock, dir, args, "")
}

/// Flip two bytes of the zlib stream of `id`'s entry, past its header and an ofs-delta's base
/// offset, so the `.idx` still names the entry and only the inflate fails.
fn corrupt_entry(stock: &str, repo: &Path, id: &str) {
    let pack_dir = repo.join(".git/objects/pack");
    let idx = std::fs::read_dir(&pack_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|x| x == "idx"))
        .expect("one pack");
    let listing = Command::new(stock)
        .arg("show-index")
        .stdin(std::fs::File::open(&idx).unwrap())
        .output()
        .unwrap();
    let offset: usize = String::from_utf8_lossy(&listing.stdout)
        .lines()
        .find_map(|l| {
            let mut f = l.split_whitespace();
            let offset = f.next()?;
            (f.next()? == id).then(|| offset.parse().unwrap())
        })
        .expect("object in the pack");
    let pack = idx.with_extension("pack");
    let mut perms = std::fs::metadata(&pack).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    std::fs::set_permissions(&pack, perms).unwrap();
    let mut data = std::fs::read(&pack).unwrap();
    let kind = (data[offset] >> 4) & 7;
    let mut at = offset;
    while data[at] & 0x80 != 0 {
        at += 1;
    }
    at += 1;
    if kind == 6 {
        while data[at] & 0x80 != 0 {
            at += 1;
        }
        at += 1;
    }
    assert_ne!(kind, 7, "repack writes ofs-deltas");
    data[at + 4] ^= 0xff;
    data[at + 5] ^= 0xff;
    std::fs::write(&pack, data).unwrap();
}

/// `f1` committed three times and `f2` once, repacked into one pack with `content(i)` as the
/// `i`th version of `f1`, then `f1` modified in the worktree.
fn history(stock: &str, root: &Path, name: &str, content: impl Fn(usize) -> String) -> PathBuf {
    setup(stock, root, &["init", "-q", "-b", "main", name]);
    let repo = root.join(name);
    setup(stock, &repo, &["config", "gc.auto", "0"]);
    setup(stock, &repo, &["config", "maintenance.auto", "false"]);
    std::fs::write(repo.join("f2"), "other\n").unwrap();
    for i in 0..3 {
        std::fs::write(repo.join("f1"), content(i)).unwrap();
        setup(stock, &repo, &["add", "-A"]);
        setup(stock, &repo, &["commit", "-q", "-m", &format!("c{i}")]);
    }
    setup(stock, &repo, &["repack", "-adfq"]);
    std::fs::write(repo.join("f1"), "dirty\n").unwrap();
    repo
}

/// Three short versions of `f1`, too small for `repack` to deltify, the newest corrupt.
fn small_blobs(stock: &str, root: &Path) -> PathBuf {
    let repo = history(stock, root, "small", |i| format!("one\ntwo {i}\nthree\n").repeat(1 + i));
    let id = setup(stock, &repo, &["rev-parse", "HEAD:f1"]);
    corrupt_entry(stock, &repo, &id);
    repo
}

/// Three versions of a 60-line `f1`, which `repack` stores as the newest whole and the older two
/// as deltas against it; `which` names the version whose entry is corrupt.
fn delta_chain(stock: &str, root: &Path, name: &str, which: &str) -> PathBuf {
    let repo = history(stock, root, name, |i| {
        let mut text: String = (1..=60 + i).map(|n| format!("line {n} of the file with some padding text\n")).collect();
        text.push_str(&format!("tail {i}\n"));
        text
    });
    let id = setup(stock, &repo, &["rev-parse", which]);
    corrupt_entry(stock, &repo, &id);
    repo
}

#[test]
fn the_stat_formats_count_a_blob_that_does_not_unpack_as_no_lines() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("stat");
    let repo = small_blobs(stock, &root);
    let (code, _, err) = same(stock, &repo, &["diff", "--stat", "HEAD~", "HEAD"]);
    // `diff_filespec_is_binary()` reads the side, and `fill_mmfile()` reads it again.
    assert_eq!((code, err.matches("error: inflate:").count()), (0, 2), "{err}");
    for args in [
        &["diff", "--numstat", "HEAD~", "HEAD"][..],
        &["diff", "--shortstat", "HEAD~", "HEAD"],
        &["diff", "--stat", "-R", "HEAD~", "HEAD"],
        &["diff", "--stat", "--summary", "HEAD~", "HEAD"],
        &["diff", "--compact-summary", "HEAD~", "HEAD"],
        &["diff-tree", "-r", "--stat", "HEAD~", "HEAD"],
        &["diff-tree", "-r", "--numstat", "HEAD~", "HEAD"],
        &["diff-tree", "-r", "--shortstat", "HEAD~", "HEAD"],
        &["show", "--stat"],
        &["show", "--numstat"],
        &["show", "--shortstat"],
        &["log", "--stat"],
        &["log", "--numstat"],
        &["log", "--shortstat"],
        &["log", "--oneline", "--stat=40"],
        &["log", "--stat", "-M", "--format=%s"],
    ] {
        same(stock, &repo, args);
    }
    // The formats that read no blob are untouched.
    for args in [&["diff", "--raw", "HEAD~", "HEAD"][..], &["show", "--raw"], &["log", "--raw"]] {
        let (code, _, err) = same(stock, &repo, args);
        assert_eq!((code, err.as_str()), (0, ""));
    }
}

#[test]
fn a_change_list_with_a_side_read_as_no_content_is_not_kept_for_the_next_run() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("ledger");
    let repo = small_blobs(stock, &root);
    let home = repo.with_extension("zvcs-home");
    let _ = std::fs::remove_dir_all(&home);
    let theirs = run_in(stock, &repo, &["log", "--stat"], "", &home);
    for run in 0..2 {
        let ours = run_in(ZVCS, &repo, &["log", "--stat"], "", &home);
        assert_eq!(ours, theirs, "run {run} with one ZVCS_HOME (left zvcs, right stock)");
    }
}

#[test]
fn a_delta_whose_base_does_not_unpack_is_read_as_no_content() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("base");
    let repo = delta_chain(stock, &root, "base", "HEAD:f1");
    // Both sides are deltas on the bad base: two empty buffers, a modification xdiff finds
    // nothing in, which `builtin_diffstat()` drops (diff.c:4245-4265).
    let (code, out, _) = same(stock, &repo, &["diff", "--stat", "HEAD~2", "HEAD~"]);
    assert_eq!((code, out.as_str()), (0, ""));
    for args in [
        &["diff", "--numstat", "HEAD~2", "HEAD~"][..],
        &["show", "--stat", "HEAD~"],
        &["log", "--stat", "--root", "HEAD~2"],
        &["diff-tree", "--stat", "--root", "-r", "HEAD~2"],
        // The read of the delta marks the base bad, so the base's own read finds no copy.
        &["diff", "--stat", "HEAD~", "HEAD"],
        &["show", "--stat"],
        &["log", "--stat"],
        &["diff-tree", "-r", "--numstat", "HEAD~", "HEAD"],
    ] {
        same(stock, &repo, args);
    }
}

#[test]
fn a_delta_whose_size_cannot_be_read_is_unable_to_read_and_missing() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("size");
    let repo = delta_chain(stock, &root, "size", "HEAD~:f1");
    let (code, _, err) = same(stock, &repo, &["log", "--stat"]);
    assert_eq!(code, 128);
    assert!(err.contains("error: delta data unpack-initial failed\nfatal: unable to read "), "{err}");
    for args in [
        &["diff", "--stat", "HEAD~", "HEAD"][..],
        &["diff", "--numstat", "HEAD~2", "HEAD~"],
        &["diff-tree", "-r", "--shortstat", "HEAD~", "HEAD"],
        &["show", "--stat", "HEAD~"],
        &["cat-file", "-t", "HEAD~:f1"],
        &["cat-file", "-s", "HEAD~:f1"],
    ] {
        same(stock, &repo, args);
    }
    let ids = format!(
        "{}\n{}\n",
        setup(stock, &repo, &["rev-parse", "HEAD:f1"]),
        setup(stock, &repo, &["rev-parse", "HEAD~:f1"])
    );
    for args in [
        &["cat-file", "--batch-check"][..],
        &["cat-file", "--batch"],
        &["cat-file", "--batch-check=%(objectsize)"],
        // The type alone is still readable.
        &["cat-file", "--batch-check=%(objectname) %(objecttype)"],
    ] {
        same_with_input(stock, &repo, args, &ids);
    }
}

#[test]
fn whole_object_reads_of_a_blob_that_does_not_unpack_still_die() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("whole");
    let repo = small_blobs(stock, &root);
    let ids = format!("{}\n", setup(stock, &repo, &["rev-parse", "HEAD:f1"]));
    for args in [
        &["cat-file", "-p", "HEAD:f1"][..],
        &["grep", "one", "HEAD"],
        &["blame", "HEAD", "--", "f1"],
        &["archive", "HEAD", "-o", "/dev/null"],
    ] {
        let (code, _, _) = same(stock, &repo, args);
        assert_eq!(code, 128, "{args:?}");
    }
    same(stock, &repo, &["cat-file", "-s", "HEAD:f1"]);
    same_with_input(stock, &repo, &["cat-file", "--batch"], &ids);
    same_with_input(stock, &repo, &["cat-file", "--batch-check"], &ids);
}
