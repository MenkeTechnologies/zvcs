//! `status` fills the untracked cache the way stock does.
//!
//! `wt_status_collect_untracked()` hands the index's cache to `read_directory()`, which lists
//! every directory whose stat, `check_only` mode and ignore files still match from the cache,
//! reads the rest from disk and records what it found (dir.c:2528-2804), validates the global
//! ignore files against the ids stored in the cache (dir.c:3087-3101), and asks for the index
//! to be written whenever it opened or invalidated anything under `core.untrackedCache=true`
//! (dir.c:3157-3171). Before, this binary created the extension on the read but never walked
//! through it, so every `status` wrote back an empty cache that stock had to rebuild from
//! scratch, and a cache stock had filled was never brought up to date.
//!
//! Each scenario starts both binaries from the *same* index file — same bytes, same mtime, in
//! the same worktree — so the comparison is of the whole written index, stat data included:
//! the cache records each directory's `stat`, the global ignore files' stat and id, and every
//! `.gitignore`'s id as `add_patterns()` computes it (the index's id for a tracked, unchanged
//! file; otherwise the file hashed with a newline appended).
//!
//! Skipped when no stock git is available.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

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

/// An index file as one binary left it: its bytes and its mtime, which is the timestamp the
/// next reader judges racy entries and directories by.
#[derive(Clone, PartialEq, Debug)]
struct Index {
    bytes: Vec<u8>,
    mtime: SystemTime,
}

/// What one `status` did: what it printed and the index it left.
#[derive(Debug)]
struct Run {
    stdout: String,
    stderr: String,
    index: Index,
}

/// Two runs agree when they printed the same and wrote the same bytes; the written file's
/// mtime is only the time of the write.
impl PartialEq for Run {
    fn eq(&self, other: &Self) -> bool {
        self.stdout == other.stdout && self.stderr == other.stderr && self.index.bytes == other.index.bytes
    }
}

impl Fixture {
    /// A repository with tracked `.gitignore`s at two levels, untracked files at the top and
    /// two levels down, an untracked directory holding only an ignored file, an empty
    /// directory and an ignored file, under the given configuration.
    fn new(tag: &str, stock: &str, config: &[(&str, &str)]) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-untr-fill-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let f = Fixture {
            work: root.join("work"),
            root,
            stock: stock.to_owned(),
        };
        std::fs::create_dir_all(f.work.join("sub")).unwrap();
        for (path, content) in [("tracked", "t\n"), (".gitignore", "*.o\n"), ("sub/s", "s\n"), ("sub/.gitignore", "x*\n")] {
            std::fs::write(f.work.join(path), content).unwrap();
        }
        f.stock(&["init", "-q", "-b", "main"]);
        f.stock(&["add", "."]);
        f.stock(&["commit", "-qm", "one"]);
        for dir in ["d/e", "u", "n/deep", "emp", "only"] {
            std::fs::create_dir_all(f.work.join(dir)).unwrap();
        }
        for (path, content) in [
            ("u/f", "x\n"),
            ("untr", "y\n"),
            ("a.o", "o\n"),
            ("n/deep/q", "q\n"),
            ("sub/xz", "z\n"),
            ("sub/w", "w\n"),
            ("only/x.o", "o\n"),
        ] {
            std::fs::write(f.work.join(path), content).unwrap();
        }
        for (key, value) in config {
            f.stock(&["config", key, value]);
        }
        f
    }

    fn run(&self, bin: &str, args: &[&str]) -> std::process::Output {
        Command::new(bin)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", self.root.join("xdg"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@example.com")
            .env("GIT_COMMITTER_NAME", "C")
            .env("GIT_COMMITTER_EMAIL", "c@example.com")
            .output()
            .unwrap()
    }

    fn stock(&self, args: &[&str]) {
        let out = self.run(&self.stock, args);
        assert!(out.status.success(), "stock `git {args:?}` failed: {out:?}");
    }

    fn index_path(&self) -> PathBuf {
        self.work.join(".git/index")
    }

    fn index(&self) -> Index {
        let path = self.index_path();
        Index {
            bytes: std::fs::read(&path).unwrap(),
            mtime: std::fs::metadata(&path).unwrap().modified().unwrap(),
        }
    }

    fn put_index(&self, index: &Index) {
        let path = self.index_path();
        std::fs::write(&path, &index.bytes).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(index.mtime)
            .unwrap();
    }

    /// `bin status <args>` starting from `from`.
    fn status_from(&self, bin: &str, from: &Index, args: &[&str]) -> Run {
        self.put_index(from);
        let mut argv = vec!["status"];
        argv.extend_from_slice(args);
        let out = self.run(bin, &argv);
        assert!(out.status.success(), "`{bin} {argv:?}` failed: {out:?}");
        Run {
            stdout: String::from_utf8(out.stdout).unwrap(),
            stderr: String::from_utf8(out.stderr).unwrap(),
            index: self.index(),
        }
    }
}

/// The number of directory blocks in the index's `UNTR` extension, `None` without one.
fn cached_directories(index: &[u8]) -> Option<u64> {
    let at = index.windows(4).position(|w| w == b"UNTR")?;
    let body = &index[at + 8..];
    let mut pos = 0;
    let ident = varint(body, &mut pos) as usize;
    // The ident, two `stat_data`, `dir_flags`, two SHA-1 ids, then the per-directory name.
    pos += ident + 72 + 4 + 2 * 20;
    pos += body[pos..].iter().position(|b| *b == 0)? + 1;
    Some(varint(body, &mut pos))
}

/// `decode_varint()` (varint.c).
fn varint(body: &[u8], pos: &mut usize) -> u64 {
    let mut c = body[*pos];
    *pos += 1;
    let mut val = u64::from(c & 127);
    while c & 128 != 0 {
        c = body[*pos];
        *pos += 1;
        val = ((val + 1) << 7) + u64::from(c & 127);
    }
    val
}

/// Run `status <args>` from the index stock committed with, with both binaries; then apply
/// `change` to the worktree and run it again from the index *stock* wrote the first time, so
/// the second round has this binary read, revalidate and rewrite a cache stock filled. Both
/// rounds must print the same and leave byte-identical indexes. Returns stock's two runs.
fn assert_like_stock(tag: &str, config: &[(&str, &str)], args: &[&str], change: impl Fn(&Path)) -> Option<(Run, Run)> {
    assert_like_stock_after(tag, config, &[], args, change)
}

/// [`assert_like_stock`] after stock has also run each of `pre` on the fixture.
fn assert_like_stock_after(
    tag: &str,
    config: &[(&str, &str)],
    pre: &[&[&str]],
    args: &[&str],
    change: impl Fn(&Path),
) -> Option<(Run, Run)> {
    let Some(stock) = stock_git() else {
        eprintln!("no stock git available; skipping");
        return None;
    };
    let f = Fixture::new(tag, stock, config);
    for step in pre {
        f.stock(step);
    }
    let start = f.index();
    let theirs = f.status_from(stock, &start, args);
    let ours = f.status_from(BIN, &start, args);
    assert_eq!(ours, theirs, "first `status {args:?}`");

    change(&f.work);
    let theirs2 = f.status_from(stock, &theirs.index, args);
    let ours2 = f.status_from(BIN, &theirs.index, args);
    assert_eq!(ours2, theirs2, "`status {args:?}` over the cache stock filled");

    // And stock reads what this binary wrote exactly as it reads its own.
    let after_ours = f.status_from(stock, &ours2.index, args);
    let after_theirs = f.status_from(stock, &theirs2.index, args);
    assert_eq!(after_ours, after_theirs, "stock `status {args:?}` over the cache this binary wrote");
    Some((theirs, theirs2))
}

const TRUE: &[(&str, &str)] = &[("core.untrackedCache", "true")];

/// The first walk fills the cache it finds empty: every directory reached, `check_only` on
/// the untracked ones, each `.gitignore`'s id, `info/exclude`'s stat and id.
#[test]
fn status_fills_an_empty_cache() {
    if let Some((first, _)) = assert_like_stock("fill", TRUE, &["--porcelain"], |_| {}) {
        assert!(cached_directories(&first.index.bytes).is_some_and(|n| n > 1), "stock filled nothing");
    }
}

/// New untracked names: the directories whose mtime moved are read again, the rest come from
/// the cache.
#[test]
fn new_untracked_names_are_picked_up() {
    assert_like_stock("new-names", TRUE, &["--porcelain"], |w| {
        std::fs::write(w.join("u/g"), "g\n").unwrap();
        std::fs::write(w.join("newroot"), "r\n").unwrap();
        std::fs::create_dir_all(w.join("brand")).unwrap();
        std::fs::write(w.join("brand/b"), "b\n").unwrap();
    });
}

/// Directories gone from the worktree drop out of the cache.
#[test]
fn removed_directories_drop_out() {
    assert_like_stock("removed", TRUE, &[], |w| {
        std::fs::remove_dir_all(w.join("u")).unwrap();
        std::fs::remove_dir_all(w.join("n")).unwrap();
    });
}

/// A changed tracked `.gitignore` is no longer the index's blob, so its id becomes the file
/// hashed with a newline appended, and the directory it governs is listed again.
#[test]
fn a_changed_tracked_gitignore_invalidates_its_directory() {
    assert_like_stock("gitignore", TRUE, &["--porcelain"], |w| {
        std::fs::write(w.join("sub/.gitignore"), "w\n").unwrap();
    });
}

/// A changed `info/exclude` invalidates every directory.
#[test]
fn a_changed_info_exclude_invalidates_everything() {
    assert_like_stock("info-exclude", TRUE, &["--porcelain"], |w| {
        std::fs::write(w.join(".git/info/exclude"), "untr\n").unwrap();
    });
}

/// `core.excludesFile`, once it exists, is validated the same way.
#[test]
fn a_new_excludes_file_invalidates_everything() {
    assert_like_stock("excludes-file", TRUE, &["--porcelain"], |w| {
        let xdg = w.parent().unwrap().join("xdg/git");
        std::fs::create_dir_all(&xdg).unwrap();
        std::fs::write(xdg.join("ignore"), "a.o\nnewroot\n").unwrap();
        std::fs::write(w.join("newroot"), "r\n").unwrap();
    });
}

/// `status.showUntrackedFiles=all` builds the cache for listing every file (`dir_flags` 0).
#[test]
fn show_untracked_files_all_fills_a_cache_for_all_files() {
    let config = &[("core.untrackedCache", "true"), ("status.showUntrackedFiles", "all")];
    assert_like_stock("all", config, &["--porcelain"], |w| {
        std::fs::write(w.join("n/deep/q2"), "q\n").unwrap();
    });
}

/// `feature.manyFiles` turns the cache on together with `index.skipHash`, whose null trailer
/// must not stop the write.
#[test]
fn feature_many_files_fills_the_cache() {
    if let Some((first, _)) = assert_like_stock("many-files", &[("feature.manyFiles", "true")], &["--porcelain"], |w| {
        std::fs::write(w.join("u/g"), "g\n").unwrap();
    }) {
        assert!(cached_directories(&first.index.bytes).is_some_and(|n| n > 1), "stock filled nothing");
    }
}

/// `core.autocrlf=true` makes `would_convert_to_git()` true for every `.gitignore`, so even a
/// tracked, unchanged one is hashed from the file rather than taken from the index.
#[test]
fn autocrlf_hashes_tracked_gitignores_from_the_file() {
    let config = &[("core.untrackedCache", "true"), ("core.autocrlf", "true")];
    assert_like_stock("autocrlf", config, &["--porcelain"], |_| {});
}

/// The walks git runs without the cache leave it as it was: `-uall` against a cache built for
/// the normal listing, `--ignored`, and a pathspec.
#[test]
fn walks_that_bypass_the_cache_leave_it_alone() {
    for (tag, args) in [
        ("bypass-uall", &["--porcelain", "-uall"][..]),
        ("bypass-ignored", &["--porcelain", "--ignored"][..]),
        ("bypass-pathspec", &["--porcelain", "--", "sub"][..]),
    ] {
        assert_like_stock(tag, TRUE, args, |w| {
            std::fs::write(w.join("u/g"), "g\n").unwrap();
        });
    }
}

/// `core.untrackedCache` unset, with a cache `update-index --untracked-cache` made: the walk
/// fills it in memory but does not ask for a write (`force_untracked_cache` is off), so it
/// reaches disk only with a write made for another reason — the racy entries, or the refresh
/// of entries an earlier write smudged — and then in the same write as that refresh.
#[test]
fn a_kept_cache_rides_along_with_the_refresh_write() {
    let pre: &[&[&str]] = &[&["update-index", "--untracked-cache"]];
    assert_like_stock_after("keep", &[], pre, &["--porcelain"], |w| {
        std::fs::write(w.join("u/g"), "g\n").unwrap();
    });
}
