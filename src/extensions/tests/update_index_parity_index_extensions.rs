//! The index extensions `update-index` leaves behind, and when it writes at all.
//!
//! * `--fsmonitor` is `add_fsmonitor()` (fsmonitor.c:759-781): a `getnanotime()`
//!   token and every entry fsmonitor-dirty, written as the `FSMN` extension
//!   (read-cache.c:3049-3059). With `core.fsmonitor` unset the next read drops it
//!   again — `tweak_fsmonitor()`'s `remove_fsmonitor()` (fsmonitor.c:818-821) marks
//!   the index changed, so even an `update-index` with nothing to do rewrites it.
//!   zvcs wrote no `FSMN` at all.
//! * A `040000` entry is a sparse-directory entry, and `set_index_entry()` marks
//!   the index `INDEX_COLLAPSED` for it (read-cache.c:134-137), so the write carries
//!   `sdir` (read-cache.c:3061-3066) outside any sparse checkout.
//! * `--clear-resolve-undo` with no `REUC` to forget changes nothing
//!   (resolve-undo.c:113-122), and `--split-index --no-split-index` is the last
//!   spelling's `remove_split_index()` on an index that was never split
//!   (split-index.c:460-495): neither writes an index, not even into a repository
//!   that has none. zvcs wrote an empty one.
//! * Writing a new shared index unlinks every other `sharedindex.*` no newer than
//!   `splitIndex.sharedIndexExpire` (`clean_shared_index_files()`,
//!   read-cache.c:3226-3253), and an expiry that resolves to the future is
//!   `git_die_config()` before the split half is written (config.c:2480-2492).
//! * A split index that is also sparse is `die("cannot write split index for a sparse
//!   index")` (read-cache.c:2999-3000), and the shared half written before it is
//!   `si->base`, whose own `sparse_index` is never set, so it carries no `sdir`.
//!
//! Every case runs stock git and zvcs on twin copies of one fixture.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Twin {
    root: PathBuf,
}

impl Drop for Twin {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn git(bin: &str, home: &Path, dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

impl Twin {
    /// Build a fixture with stock git through `setup`, then copy it so stock and zvcs each
    /// get their own.
    fn new(tag: &str, stock: &str, setup: impl Fn(&dyn Fn(&[&str]), &Path)) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-uiext-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let stock_dir = root.join("stock");
        std::fs::create_dir_all(&stock_dir).unwrap();
        let run = |args: &[&str]| {
            let (_, err, code) = git(stock, &root, &stock_dir, args);
            assert_eq!(code, 0, "fixture `git {args:?}`: {err}");
        };
        run(&["-c", "maintenance.auto=false", "init", "-q", "-b", "main", "."]);
        setup(&run, &stock_dir);
        let status = Command::new("cp").arg("-Rp").arg(&stock_dir).arg(root.join("zvcs")).status().unwrap();
        assert!(status.success());
        Twin { root }
    }

    fn dir(&self, which: &str) -> PathBuf {
        self.root.join(which)
    }

    /// The same command through both binaries, each in its own copy.
    fn both(&self, stock: &str, args: &[&str]) -> [(String, String, i32); 2] {
        [git(stock, &self.root, &self.dir("stock"), args), git(BIN, &self.root, &self.dir("zvcs"), args)]
    }

    fn files(&self, which: &str, prefix: &str) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.dir(which).join(".git"))
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.starts_with(prefix))
            .collect();
        names.sort();
        names
    }
}

/// The extensions of a version 2/3 index file: each signature with its size, except that
/// `FSMN`'s token is a timestamp and only its shape is kept.
fn extensions(index: &Path) -> Option<Vec<String>> {
    let data = std::fs::read(index).ok()?;
    let be32 = |at: usize| u32::from_be_bytes(data[at..at + 4].try_into().unwrap()) as usize;
    let version = be32(4);
    assert!(version == 2 || version == 3, "a v{version} index is not parsed here");
    let mut at = 12;
    for _ in 0..be32(8) {
        let flags = u16::from_be_bytes(data[at + 60..at + 62].try_into().unwrap());
        let fixed = if version == 3 && flags & 0x4000 != 0 { 64 } else { 62 };
        let name_len = data[at + fixed..].iter().position(|&b| b == 0).unwrap();
        at += (fixed + name_len + 8) & !7;
    }
    let mut out = Vec::new();
    while at < data.len() - 20 {
        let sig = String::from_utf8_lossy(&data[at..at + 4]).into_owned();
        let size = be32(at + 4);
        let body = &data[at + 8..at + 8 + size];
        out.push(if sig == "FSMN" {
            // Version 2, a NUL-terminated token of digits, then the EWAH bitmap.
            let token_end = 4 + body[4..].iter().position(|&b| b == 0).unwrap();
            let digits = body[4..token_end].iter().all(u8::is_ascii_digit);
            format!("FSMN v{} digits={digits} ewah={:?}", u32::from_be_bytes(body[..4].try_into().unwrap()), &body[token_end + 1..])
        } else {
            format!("{sig}:{size}")
        });
        at += 8 + size;
    }
    Some(out)
}

fn committed(run: &dyn Fn(&[&str]), dir: &Path) {
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    std::fs::create_dir_all(dir.join("d")).unwrap();
    std::fs::write(dir.join("d/b.txt"), "b\n").unwrap();
    std::fs::write(dir.join("c.txt"), "c\n").unwrap();
    run(&["add", "-A"]);
    run(&["-c", "maintenance.auto=false", "commit", "-q", "-m", "one"]);
}

#[test]
fn fsmonitor_writes_the_extension_and_the_next_write_drops_it() {
    let Some(stock) = stock_git() else { return };
    let t = Twin::new("fsmn", stock, committed);
    let [s, z] = t.both(stock, &["update-index", "--verbose", "--fsmonitor"]);
    assert_eq!(z, s);
    let ext = extensions(&t.dir("stock").join(".git/index")).unwrap();
    assert!(ext.iter().any(|e| e.starts_with("FSMN v2 digits=true")), "{ext:?}");
    assert_eq!(extensions(&t.dir("zvcs").join(".git/index")), Some(ext));

    // Nothing asked of it, but the read dropped the extension and so must write.
    let [s, z] = t.both(stock, &["update-index"]);
    assert_eq!(z, s);
    let ext = extensions(&t.dir("stock").join(".git/index")).unwrap();
    assert!(!ext.iter().any(|e| e.starts_with("FSMN")), "{ext:?}");
    assert_eq!(extensions(&t.dir("zvcs").join(".git/index")), Some(ext));
}

#[test]
fn a_tree_entry_marks_the_index_sparse() {
    let Some(stock) = stock_git() else { return };
    let t = Twin::new("sdir", stock, committed);
    let empty_tree = "040000,4b825dc642cb6eb9a060e54bf8d69288fbee4904,tr";
    let [s, z] = t.both(stock, &["update-index", "--add", "--cacheinfo", empty_tree]);
    assert_eq!(z, s);
    let ext = extensions(&t.dir("stock").join(".git/index")).unwrap();
    assert!(ext.contains(&"sdir:0".to_owned()), "{ext:?}");
    assert_eq!(extensions(&t.dir("zvcs").join(".git/index")), Some(ext));
    let [s, z] = t.both(stock, &["ls-files", "-s", "--sparse"]);
    assert_eq!(z, s);
}

#[test]
fn nothing_to_change_writes_no_index() {
    let Some(stock) = stock_git() else { return };
    for (tag, args) in [
        ("cru", &["update-index", "--clear-resolve-undo"][..]),
        ("nosplit", &["update-index", "--split-index", "--no-split-index"][..]),
    ] {
        let t = Twin::new(tag, stock, |_, dir| std::fs::write(dir.join("u.txt"), "u\n").unwrap());
        let [s, z] = t.both(stock, args);
        assert_eq!(z, s, "{args:?}");
        assert!(!t.dir("stock").join(".git/index").exists(), "{args:?}");
        assert!(!t.dir("zvcs").join(".git/index").exists(), "{args:?}");
    }
}

/// A split index whose shared halves are a year old.
fn split_with_old_shared(run: &dyn Fn(&[&str]), dir: &Path) {
    committed(run, dir);
    run(&["update-index", "--split-index"]);
    std::fs::write(dir.join("e.txt"), "e\n").unwrap();
    run(&["-c", "splitIndex.maxPercentChange=0", "update-index", "--add", "e.txt"]);
    for entry in std::fs::read_dir(dir.join(".git")).unwrap() {
        let path = entry.unwrap().path();
        if path.file_name().unwrap().to_string_lossy().starts_with("sharedindex.") {
            let status = Command::new("touch").args(["-t", "202001010000"]).arg(&path).status().unwrap();
            assert!(status.success());
        }
    }
}

#[test]
fn a_new_shared_index_reaps_the_expired_ones() {
    let Some(stock) = stock_git() else { return };
    let t = Twin::new("reap", stock, split_with_old_shared);
    assert!(t.files("stock", "sharedindex.").len() >= 2, "{:?}", t.files("stock", "sharedindex."));
    let [s, z] = t.both(stock, &["update-index", "--split-index"]);
    assert_eq!(z, s);
    assert_eq!(t.files("zvcs", "sharedindex."), t.files("stock", "sharedindex."));

    // `never` keeps every one of them.
    let t = Twin::new("never", stock, split_with_old_shared);
    let before = t.files("stock", "sharedindex.");
    let [s, z] = t.both(stock, &["-c", "splitIndex.sharedIndexExpire=never", "update-index", "--split-index"]);
    assert_eq!(z, s);
    assert_eq!(t.files("stock", "sharedindex."), before);
    assert_eq!(t.files("zvcs", "sharedindex."), before);
}

#[test]
fn an_expiry_in_the_future_dies_before_the_split_half_is_written() {
    let Some(stock) = stock_git() else { return };
    let t = Twin::new("badexp", stock, split_with_old_shared);
    let index_before = std::fs::read(t.dir("stock").join(".git/index")).unwrap();
    let [s, z] = t.both(stock, &["-c", "splitIndex.sharedIndexExpire=garbage", "update-index", "--split-index"]);
    assert_eq!(s.2, 128, "{s:?}");
    assert_eq!(z, s);
    assert_eq!(std::fs::read(t.dir("stock").join(".git/index")).unwrap(), index_before);
    assert_eq!(std::fs::read(t.dir("zvcs").join(".git/index")).unwrap(), index_before);
    assert!(!t.dir("zvcs").join(".git/index.lock").exists());
    assert_eq!(t.files("zvcs", "sharedindex."), t.files("stock", "sharedindex."));
}

#[test]
fn a_split_index_cannot_be_written_sparse() {
    let Some(stock) = stock_git() else { return };
    let t = Twin::new("splitsdir", stock, split_with_old_shared);
    let empty_tree = "040000,4b825dc642cb6eb9a060e54bf8d69288fbee4904,tr";
    let [s, z] = t.both(stock, &["update-index", "--add", "--cacheinfo", empty_tree]);
    assert_eq!(s.1, "fatal: cannot write split index for a sparse index\n", "{s:?}");
    assert_eq!(z, s);
    // The new shared half was already in place, and carries no `sdir` of its own.
    assert_eq!(t.files("zvcs", "sharedindex."), t.files("stock", "sharedindex."));
    assert!(!t.dir("zvcs").join(".git/index.lock").exists());
}
