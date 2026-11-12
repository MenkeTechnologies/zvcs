//! Where `read-tree -m` loads an `info/sparse-checkout` cone mode cannot read, relative to
//! the merge's refusal.
//!
//! `unpack_trees()` reads the pattern list before its merge function looks at a single entry
//! (unpack-trees.c:1931-1936), and `add_pattern_to_hashsets()` warns as it does
//! (dir.c:798-936), so the pair precedes `error: Entry '<path>' not uptodate. Cannot merge.`
//! for one tree and two alike. The merge walks the paths in order and stops at the first it
//! refuses, so a racily clean entry after it is never hashed and the source index never loads
//! the file. Without `-u` the unpack loads no list, and the result that replaced the index
//! loads it when the index write smudges a racily clean entry (read-cache.c:2902).
//!
//! Every file is dated 2020. "racy" refreshes the index and dates it into the same second,
//! so every entry is racily clean; "dirty" leaves the index's stat data behind the files.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Twin {
    root: PathBuf,
}

impl Drop for Twin {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn run(bin: &str, dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
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

const FILES: [&str; 4] = ["top", "a/x", "a/b/y", "c/z"];

#[derive(Clone, Copy, PartialEq)]
enum Stat {
    Racy,
    Dirty,
}

fn date_2020(path: &Path) {
    let past = SystemTime::UNIX_EPOCH + Duration::from_secs(1_577_836_800);
    std::fs::File::options().write(true).open(path).unwrap().set_modified(past).unwrap();
}

/// `main`, two commits over `top`, `a/x`, `a/b/y` and `c/z` (the second changing all but
/// `a/b/y`), in a cone-mode sparse checkout whose pattern file holds only `/a/*`; then
/// `dirty` written over `a/x`, a change of size `verify_uptodate()` sees from the stat data.
fn prepare(stock: &str, r: &Path, stat: Stat) {
    std::fs::create_dir_all(r.join("a/b")).unwrap();
    std::fs::create_dir_all(r.join("c")).unwrap();
    for (path, body) in FILES.iter().zip(["1\n", "2\n", "3\n", "4\n"]) {
        std::fs::write(r.join(path), body).unwrap();
    }
    let ok = |args: &[&str]| assert_eq!(run(stock, r, args).2, 0, "{args:?}");
    ok(&["init", "-q", "-b", "main", "."]);
    ok(&["add", "."]);
    ok(&["-c", "maintenance.auto=false", "commit", "-q", "-m", "one"]);
    for (path, body) in [("top", "11\n"), ("a/x", "22\n"), ("c/z", "44\n")] {
        std::fs::write(r.join(path), body).unwrap();
    }
    ok(&["-c", "maintenance.auto=false", "commit", "-q", "-a", "-m", "two"]);
    for path in FILES {
        date_2020(&r.join(path));
    }
    if stat == Stat::Racy {
        ok(&["update-index", "--refresh"]);
        date_2020(&r.join(".git/index"));
    }
    ok(&["config", "core.sparseCheckout", "true"]);
    ok(&["config", "core.sparseCheckoutCone", "true"]);
    std::fs::write(r.join(".git/info/sparse-checkout"), "/a/*\n").unwrap();
}

fn compare(tag: &str, stat: Stat, dirty: bool, args: &[&str]) {
    let Some(stock) = stock_git::stock_git() else {
        return;
    };
    let base = std::env::temp_dir().join(format!("zvcs-rt-cone-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let _twin = Twin { root: base.clone() };
    let mut outputs = Vec::new();
    for (side, bin) in [("s", stock), ("z", BIN)] {
        let r = base.join(side);
        std::fs::create_dir_all(&r).unwrap();
        prepare(stock, &r, stat);
        if dirty {
            std::fs::write(r.join("a/x"), "dirty\n").unwrap();
        }
        let out = run(bin, &r, args);
        outputs.push((out, run(stock, &r, &["ls-files", "-s", "-t"])));
    }
    assert_eq!(outputs[1], outputs[0], "{tag}: {args:?}");
}

#[test]
fn the_unpack_warns_before_the_merge_refuses() {
    compare("one-tree", Stat::Dirty, true, &["read-tree", "-m", "-u", "HEAD~1"]);
    compare("two-trees", Stat::Dirty, true, &["read-tree", "-m", "-u", "HEAD", "HEAD~1"]);
}

#[test]
fn the_merge_hashes_nothing_after_the_path_it_refuses() {
    // `c/z` and `top` are racily clean but come after `a/x`.
    compare("racy-refused", Stat::Racy, true, &["read-tree", "-m", "-u", "HEAD~1"]);
}

#[test]
fn a_merge_without_u_loads_its_source_and_then_its_result() {
    compare("racy-no-u", Stat::Racy, false, &["read-tree", "-m", "HEAD~1"]);
    compare("racy-two-trees", Stat::Racy, false, &["read-tree", "-m", "-u", "HEAD", "HEAD~1"]);
}
