//! Commands git runs over one in-memory index, written once, against the split index stock
//! writes.
//!
//! A split index keeps an entry in the shared half unless something makes it rewrite it in the
//! split half, and one of those things is a racily clean entry that nothing has verified:
//! `prepare_to_write_split_index()` moves `!ce_uptodate(ce) && is_racy_timestamp(istate, ce)`
//! into the split half so `do_write_index()` can smudge it (split-index.c:283-294). A refresh
//! verifies every racy entry by content and marks it up to date, so git, which goes on to write
//! the very index it refreshed, writes none of them. A port that writes the refreshed index and
//! reads it back has lost those marks, and its second write turned each racy entry into a
//! stand-in in the split half.
//!
//! Each case builds two repositories with stock git — the same commands, so the same entries —
//! one of whose files carries an mtime far in the future, which keeps its entry racy against
//! every index, and runs the same command with stock in one and this binary in the other. The
//! index files are compared byte for byte with only the filesystem facts masked: `ctime`, `mtime` (a file the
//! command writes is dated by the second it ran in), `dev`, `ino`, `uid`, `gid`, the shared index's id in `link`, and the
//! checksum.
//!
//! Skipped when no stock git is available.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn ok(bin: &str, dir: &Path, home: &Path, args: &[&str]) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .unwrap();
    assert!(out.status.success(), "`{bin} {args:?}` failed: {out:?}");
}

/// The index file with every entry's filesystem facts zeroed, the `link` extension's shared
/// index id zeroed, and the checksum dropped.
fn masked_index(dir: &Path) -> Vec<u8> {
    let mut b = std::fs::read(dir.join(".git/index")).unwrap();
    assert_eq!(&b[..4], b"DIRC");
    let n = u32::from_be_bytes(b[8..12].try_into().unwrap());
    let mut pos = 12;
    for _ in 0..n {
        b[pos..pos + 24].fill(0);
        b[pos + 28..pos + 36].fill(0);
        let flags = u16::from_be_bytes([b[pos + 60], b[pos + 61]]);
        let name_at = pos + 62 + if flags & 0x4000 != 0 { 2 } else { 0 };
        let nul = b[name_at..].iter().position(|c| *c == 0).unwrap();
        pos += (name_at + nul + 1 - pos).div_ceil(8) * 8;
    }
    let end = b.len() - 20;
    while pos < end {
        let len = u32::from_be_bytes(b[pos + 4..pos + 8].try_into().unwrap()) as usize;
        if &b[pos..pos + 4] == b"link" {
            b[pos + 8..pos + 28].fill(0);
        }
        pos += 8 + len;
    }
    b.truncate(end);
    b
}

fn shared_index_count(dir: &Path) -> usize {
    std::fs::read_dir(dir.join(".git"))
        .unwrap()
        .filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().starts_with("sharedindex."))
        .count()
}

impl Fixture {
    /// `a`..`e` committed under `core.splitIndex=true`, `a` with an mtime in 2099, the rest
    /// refreshed and the index split afresh;
    /// `side` changes `b`, `main` changes `c`.
    fn new(stock: &str, tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-split-one-write-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let f = Fixture { root };
        for side in ["stock", "ours"] {
            let dir = f.root.join(side);
            std::fs::create_dir_all(&dir).unwrap();
            let run = |args: &[&str]| ok(stock, &dir, &f.root, args);
            run(&["init", "-q", "-b", "main", "."]);
            run(&["config", "core.splitIndex", "true"]);
            for p in ["a", "b", "c", "d", "e"] {
                std::fs::write(dir.join(p), format!("{p}\n")).unwrap();
            }
            let future = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(4_070_908_800);
            std::fs::File::options()
                .write(true)
                .open(dir.join("a"))
                .unwrap()
                .set_modified(future)
                .unwrap();
            run(&["add", "."]);
            run(&["commit", "-q", "-m", "one"]);
            run(&["update-index", "--split-index"]);
            run(&["checkout", "-q", "-b", "side"]);
            std::fs::write(dir.join("b"), "side\n").unwrap();
            run(&["commit", "-q", "-a", "-m", "side"]);
            run(&["checkout", "-q", "main"]);
            std::fs::write(dir.join("c"), "main\n").unwrap();
            run(&["commit", "-q", "-a", "-m", "main"]);
            // Every file but `a` older than the index, which is then split afresh: `a` is
            // the one racy entry, whatever second the commands above ran in.
            let past = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_577_836_800);
            for p in ["b", "c", "d", "e"] {
                std::fs::File::options().write(true).open(dir.join(p)).unwrap().set_modified(past).unwrap();
            }
            run(&["update-index", "-q", "--refresh"]);
            run(&["update-index", "--split-index"]);
        }
        f
    }

    fn both(&self, stock: &str, args: &[&str]) -> ((Vec<u8>, usize), (Vec<u8>, usize)) {
        let (s, o) = (self.root.join("stock"), self.root.join("ours"));
        ok(stock, &s, &self.root, args);
        ok(BIN, &o, &self.root, args);
        ((masked_index(&s), shared_index_count(&s)), (masked_index(&o), shared_index_count(&o)))
    }
}

#[test]
fn a_branch_switch_writes_the_index_it_refreshed_once() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, "checkout");
    let (theirs, ours) = f.both(stock, &["checkout", "-q", "side"]);
    assert_eq!(ours, theirs);
}

#[test]
fn switch_writes_the_index_it_refreshed_once() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, "switch");
    let (theirs, ours) = f.both(stock, &["switch", "-q", "side"]);
    assert_eq!(ours, theirs);
}

#[test]
fn a_switch_that_moves_nothing_still_writes_the_refreshed_index() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, "noop");
    let (theirs, ours) = f.both(stock, &["checkout", "-q", "main"]);
    assert_eq!(ours, theirs);
}

#[test]
fn a_non_fast_forward_merge_keeps_the_index_split() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, "merge");
    let (theirs, ours) = f.both(stock, &["merge", "-q", "--no-edit", "side"]);
    assert_eq!(ours, theirs);
}

/// Rewrite `d` in both copies with an old mtime: a change for `stash` to take.
fn edit_d(f: &Fixture) {
    for side in ["stock", "ours"] {
        let path = f.root.join(side).join("d");
        std::fs::write(&path, "x\n").unwrap();
        let past = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_577_840_400);
        std::fs::File::options().write(true).open(&path).unwrap().set_modified(past).unwrap();
    }
}

/// `stash push` writes the index three times before its child `reset --hard` writes it again,
/// and its temporary index — split under `core.splitIndex=true` — leaves a shared index of its
/// own behind.
#[test]
fn stash_push_writes_the_index_as_often_as_stock() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, "stash-push");
    edit_d(&f);
    let (theirs, ours) = f.both(stock, &["stash", "-q"]);
    assert_eq!(ours, theirs);
}

/// `stash pop` writes the refreshed index, the merge's and the unstaged one in turn, each
/// deciding its split half on its own.
#[test]
fn stash_pop_writes_the_index_as_often_as_stock() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, "stash-pop");
    edit_d(&f);
    for side in ["stock", "ours"] {
        ok(stock, &f.root.join(side), &f.root, &["stash", "-q"]);
    }
    let (theirs, ours) = f.both(stock, &["stash", "pop", "-q"]);
    assert_eq!(ours, theirs);
}
