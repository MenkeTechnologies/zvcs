//! The untracked cache (`UNTR`) a stock git built must survive every index this binary
//! writes, and must be invalidated on the way exactly where stock invalidates it.
//!
//! Before, the writer had no `UNTR` arm at all: one `status` or `add` here rewrote the index
//! without the extension, silently throwing away the cache stock had built. Carrying it is only
//! half of it, though — a cache written back *without* invalidation is worse than none,
//! because stock trusts a directory whose `stat` still matches and would list a file this
//! binary just added as untracked. git invalidates in two places:
//!
//! * `add_index_entry_with_check()` for a name it did not hold, and `remove_file_from_index()`,
//!   `remove_marked_cache_entries()` and `rename_index_entry_at()` for a name it drops
//!   (read-cache.c:1270-1271, :635, :614, :170) — the *name* came or went;
//! * `unpack_trees()`'s `invalidate_ce_path()` (unpack-trees.c:2296-2303) for every entry a
//!   checkout or hard reset adds, removes or replaces — the *content* moved too.
//!
//! Each call runs `invalidate_one_component()` (dir.c:3992-4013), which clears the leaf
//! directory and, under `DIR_SHOW_OTHER_DIRECTORIES` (what `status` builds with), every
//! directory above it.
//!
//! Every scenario is run twice on a fresh fixture at the *same* path — the cache's ident is
//! `Location <worktree>, system <uname>` and stock recreates a cache whose ident does not match
//! — once with stock git doing the command and once with this binary, and the two resulting
//! caches are compared with their stat data stepped over (the files are recreated each run).
//! Stock's own `status` is then asked about both repositories, with and without
//! `--untracked-files=all`, which is the correctness half: a cache left valid where it should
//! not be shows up there as a wrong untracked listing.
//!
//! Skipped when no stock git is available.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A stock git to build the cache with and to read the result back, or `None` to skip.
fn stock_git() -> Option<String> {
    if let Ok(p) = std::env::var("ZVCS_STOCK_GIT") {
        return Path::new(&p).exists().then_some(p);
    }
    ["/opt/homebrew/bin/git", "/usr/bin/git", "/usr/local/bin/git"]
        .into_iter()
        .find(|p| Path::new(p).exists())
        .map(str::to_owned)
}

struct Fixture {
    root: PathBuf,
    work: PathBuf,
    stock: String,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str, stock: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-untr-{tag}-{}", std::process::id()));
        let f = Fixture {
            work: root.join("work"),
            root,
            stock: stock.to_owned(),
        };
        f.reset();
        f
    }

    /// (Re)build the repository at the fixture's one path, with a populated untracked cache:
    /// `main` holds `tracked`, `d/e/f` and `d/g`; `side` changes `d/e/f` and adds `d/new`;
    /// `sub/u`, `untr`, `d/e/untr2` and `new/deep/q` are untracked, and `tracked` is modified.
    fn reset(&self) {
        let _ = std::fs::remove_dir_all(&self.root);
        std::fs::create_dir_all(self.work.join("d/e")).unwrap();
        for (path, content) in [("tracked", "t\n"), ("d/e/f", "x\n"), ("d/g", "g\n"), (".gitignore", "*.o\n")] {
            std::fs::write(self.work.join(path), content).unwrap();
        }
        self.stock(&["init", "-q", "-b", "main"]);
        self.stock(&["add", "."]);
        self.stock(&["commit", "-qm", "one"]);
        self.stock(&["checkout", "-qb", "side"]);
        std::fs::write(self.work.join("d/e/f"), "side\n").unwrap();
        std::fs::write(self.work.join("d/new"), "n\n").unwrap();
        self.stock(&["add", "d"]);
        self.stock(&["commit", "-qm", "side"]);
        self.stock(&["checkout", "-q", "main"]);
        std::fs::create_dir_all(self.work.join("sub")).unwrap();
        std::fs::create_dir_all(self.work.join("new/deep")).unwrap();
        for (path, content) in [
            ("sub/u", "u\n"),
            ("untr", "z\n"),
            ("d/e/untr2", "w\n"),
            ("new/deep/q", "q\n"),
            ("tracked", "t\nmodified\n"),
        ] {
            std::fs::write(self.work.join(path), content).unwrap();
        }
        self.stock(&["config", "core.untrackedCache", "true"]);
        // The first walk creates the cache, the second fills and writes it.
        self.stock(&["status", "--porcelain"]);
        self.stock(&["status", "--porcelain"]);
        assert!(
            untracked_cache(&self.index()).is_some_and(|c| c.contains("blocks=")),
            "stock wrote no populated untracked cache to start from"
        );
    }

    fn run(&self, bin: &str, args: &[&str]) -> std::process::Output {
        Command::new(bin)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@example.com")
            .env("GIT_COMMITTER_NAME", "C")
            .env("GIT_COMMITTER_EMAIL", "c@example.com")
            .output()
            .unwrap()
    }

    fn stock(&self, args: &[&str]) -> String {
        let out = self.run(&self.stock, args);
        assert!(out.status.success(), "stock `git {args:?}` failed: {out:?}");
        String::from_utf8(out.stdout).unwrap()
    }

    fn index(&self) -> Vec<u8> {
        std::fs::read(self.work.join(".git/index")).unwrap()
    }

    /// Run `args` with `bin` on a fresh fixture and report the cache it left behind and what
    /// stock's `status` then says about the repository.
    fn outcome(&self, bin: &str, args: &[&str]) -> (Option<String>, String, String) {
        self.reset();
        let out = self.run(bin, args);
        assert!(out.status.success(), "`{bin} {args:?}` failed: {out:?}");
        let cache = untracked_cache(&self.index());
        // `-uall` first: it walks with different `dir_flags`, which stock answers by building a
        // new cache — so the default listing, which *uses* the cache, has to be read before.
        let normal = self.stock(&["status", "--porcelain"]);
        let all = self.stock(&["status", "--porcelain", "--untracked-files=all"]);
        (cache, normal, all)
    }
}

/// The `UNTR` body rendered without its stat data: `dir_flags`, the exclude-file hashes, the
/// per-directory file name, then every directory block with its untracked names and the three
/// bitmaps. `None` when the index has no untracked cache.
fn untracked_cache(index: &[u8]) -> Option<String> {
    let at = index.windows(4).position(|w| w == b"UNTR")?;
    let len = u32::from_be_bytes(index[at + 4..at + 8].try_into().unwrap()) as usize;
    let body = &index[at + 8..at + 8 + len];
    let mut pos = 0usize;
    // `decode_varint()` (varint.c).
    let varint = |pos: &mut usize| -> u64 {
        let mut c = body[*pos];
        *pos += 1;
        let mut val = u64::from(c & 127);
        while c & 128 != 0 {
            c = body[*pos];
            *pos += 1;
            val = ((val + 1) << 7) + u64::from(c & 127);
        }
        val
    };
    let cstr = |pos: &mut usize| -> String {
        let end = *pos + body[*pos..].iter().position(|b| *b == 0).unwrap();
        let s = String::from_utf8_lossy(&body[*pos..end]).into_owned();
        *pos = end + 1;
        s
    };
    let ident_len = varint(&mut pos) as usize;
    // The ident, then the two 36-byte `stat_data` records of the exclude files.
    pos += ident_len + 72;
    let mut out = format!("flags={:08x}", u32::from_be_bytes(body[pos..pos + 4].try_into().unwrap()));
    pos += 4;
    for _ in 0..2 {
        out += &format!(" {}", hex(&body[pos..pos + 20]));
        pos += 20;
    }
    out += &format!(" per-dir={}", cstr(&mut pos));
    let blocks = varint(&mut pos);
    out += &format!(" blocks={blocks}");
    if blocks == 0 {
        return Some(out);
    }
    for _ in 0..blocks {
        let untracked = varint(&mut pos);
        let dirs = varint(&mut pos);
        let name = cstr(&mut pos);
        let names: Vec<String> = (0..untracked).map(|_| cstr(&mut pos)).collect();
        out += &format!("\n  [{name}] dirs={dirs} untracked={names:?}");
    }
    for which in ["valid", "check_only", "sha1_valid"] {
        let words = u32::from_be_bytes(body[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let end = pos + 12 + 8 * words;
        out += &format!("\n  {which}={}", hex(&body[pos..end]));
        pos = end;
    }
    Some(out)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn assert_like_stock(tag: &str, args: &[&str]) {
    let Some(stock) = stock_git() else {
        eprintln!("no stock git available; skipping");
        return;
    };
    let f = Fixture::new(tag, &stock);
    let theirs = f.outcome(&stock, args);
    let ours = f.outcome(BIN, args);
    assert!(theirs.0.is_some(), "stock dropped its own cache on `{args:?}`");
    assert_eq!(ours.0, theirs.0, "untracked cache after `{args:?}`");
    assert_eq!(ours.1, theirs.1, "stock `status` after `{args:?}`");
    assert_eq!(ours.2, theirs.2, "stock `status -uall` after `{args:?}`");
}

/// The stock-built cache comes back byte for byte when nothing about the entries changed.
#[test]
fn an_untouched_cache_is_written_back_byte_for_byte() {
    let Some(stock) = stock_git() else {
        eprintln!("no stock git available; skipping");
        return;
    };
    let f = Fixture::new("verbatim", &stock);
    let before = f.index();
    let out = f.run(BIN, &["update-index", "--force-write-index"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(f.index(), before);
}

/// A new name at the top: the root directory loses its listing, nothing else does.
#[test]
fn add_invalidates_the_directory_of_the_new_name() {
    assert_like_stock("add", &["add", "untr"]);
}

/// A new name two levels down: with `DIR_SHOW_OTHER_DIRECTORIES` every ancestor goes too,
/// since `new/` may stop being an untracked directory as a whole.
#[test]
fn add_of_a_nested_name_invalidates_every_ancestor() {
    assert_like_stock("add-deep", &["add", "new/deep/q"]);
}

#[test]
fn rm_cached_invalidates_the_directory_of_the_dropped_name() {
    assert_like_stock("rm", &["rm", "-q", "--cached", "d/g"]);
}

#[test]
fn mv_invalidates_both_the_old_and_the_new_name() {
    assert_like_stock("mv", &["mv", "tracked", "d/e/moved"]);
}

/// Staging a change to a name the index already holds replaces the entry in place, which
/// leaves the cache alone (`replace_index_entry()`, read-cache.c:1263-1267).
#[test]
fn commit_all_of_existing_names_keeps_the_cache_valid() {
    assert_like_stock("commit-a", &["commit", "-qam", "all"]);
}

/// `read_from_tree()` stages entry by entry: `d/new` is a new name and invalidates `d` and its
/// ancestors, while `d/e/f` only changes content and leaves `d/e` valid.
#[test]
fn a_mixed_reset_invalidates_only_the_names_it_adds_or_drops() {
    assert_like_stock("reset-mixed", &["reset", "-q", "side"]);
}

/// `unpack_trees()` invalidates content changes as well: `d/e` goes this time.
#[test]
fn a_hard_reset_invalidates_every_entry_it_rewrote() {
    assert_like_stock("reset-hard", &["reset", "-q", "--hard", "side"]);
}

#[test]
fn a_branch_switch_invalidates_every_entry_it_rewrote() {
    assert_like_stock("checkout", &["checkout", "-q", "side"]);
}
