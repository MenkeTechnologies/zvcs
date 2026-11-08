//! The cone-loading warnings of a sparse `unpack_trees()` whose `info/sparse-checkout`
//! cone mode cannot read, for `reset`, `read-tree -u`, `checkout` and `switch`.
//!
//! `add_pattern_to_hashsets()` (dir.c:798-936) warns each time the file is loaded in
//! cone mode: once by `populate_from_existing_patterns()` for the run's own pattern
//! list (unpack-trees.c:1832-1840), and once more by `init_sparse_checkout_patterns()`
//! of the result index (dir.c:1557-1574) when `check_updates()` writes a regular file
//! and its attribute lookup reads `.gitattributes` from that index (entry.c:584-587,
//! attr.c:821). zvcs printed neither.
//!
//! The work tree files are dated in the past and the index refreshed over them, so no
//! entry is racily clean and no content comparison adds a load of its own: the count
//! depends only on whether the command writes a file.

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
        .env("GIT_EDITOR", "true")
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

/// `main` (two commits over `top`, `a/x`, `a/b/y`, `c/z`) and `other` at the first, in a
/// cone-mode sparse checkout whose pattern file is `patterns`, its files dated 2020.
fn prepare(stock: &str, r: &Path, patterns: &str) {
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
    ok(&["branch", "other", "HEAD~1"]);
    let past = SystemTime::UNIX_EPOCH + Duration::from_secs(1_577_836_800);
    for path in FILES {
        std::fs::File::options()
            .write(true)
            .open(r.join(path))
            .unwrap()
            .set_modified(past)
            .unwrap();
    }
    ok(&["update-index", "--refresh"]);
    ok(&["config", "core.sparseCheckout", "true"]);
    ok(&["config", "core.sparseCheckoutCone", "true"]);
    std::fs::write(r.join(".git/info/sparse-checkout"), patterns).unwrap();
}

/// Run `args` on twin fixtures, then stock `ls-files -t` (which compares no content),
/// and compare both runs and which files are on disk afterwards.
fn compare(tag: &str, patterns: &str, args: &[&str]) {
    let Some(stock) = stock_git::stock_git() else {
        return;
    };
    let base = std::env::temp_dir().join(format!("zvcs-reset-cone-warnings-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let _twin = Twin { root: base.clone() };
    let mut outputs = Vec::new();
    for (side, bin) in [("s", stock), ("z", BIN)] {
        let r = base.join(side);
        std::fs::create_dir_all(&r).unwrap();
        prepare(stock, &r, patterns);
        let mut out = vec![run(bin, &r, args), run(stock, &r, &["ls-files", "-t"])];
        for path in FILES {
            out.push((path.to_owned(), std::fs::read_to_string(r.join(path)).unwrap_or_default(), 0));
        }
        outputs.push(out);
    }
    assert_eq!(outputs[1], outputs[0], "{tag}: {args:?}");
}

const GLOB: &str = "/a/*\n";
const PLAIN_DIR: &str = "/*\n!/*/\na/\n";
const EXCLUDES_ALL: &str = "junk\n";

#[test]
fn a_sparse_unpack_that_writes_a_file_loads_the_patterns_twice() {
    for (tag, patterns) in [("glob", GLOB), ("plain-dir", PLAIN_DIR)] {
        compare(&format!("writes-{tag}-hard"), patterns, &["reset", "--hard", "HEAD~1"]);
        compare(&format!("writes-{tag}-keep"), patterns, &["reset", "--keep", "other"]);
        compare(&format!("writes-{tag}-merge"), patterns, &["reset", "--merge", "other"]);
        compare(&format!("writes-{tag}-read-tree"), patterns, &["read-tree", "-m", "-u", "HEAD~1"]);
        compare(&format!("writes-{tag}-read-tree-reset"), patterns, &["read-tree", "--reset", "-u", "HEAD~1"]);
        compare(&format!("writes-{tag}-read-tree-2"), patterns, &["read-tree", "-m", "-u", "HEAD", "other"]);
        compare(&format!("writes-{tag}-checkout"), patterns, &["checkout", "other"]);
        compare(&format!("writes-{tag}-detach"), patterns, &["checkout", "-q", "--detach", "other"]);
        compare(&format!("writes-{tag}-switch"), patterns, &["switch", "-q", "-C", "nb", "other"]);
    }
}

#[test]
fn a_sparse_unpack_that_writes_nothing_loads_the_patterns_once() {
    for (tag, patterns) in [("glob", GLOB), ("plain-dir", PLAIN_DIR)] {
        compare(&format!("still-{tag}-hard"), patterns, &["reset", "--hard"]);
        compare(&format!("still-{tag}-read-tree"), patterns, &["read-tree", "-m", "-u", "HEAD"]);
        compare(&format!("still-{tag}-checkout"), patterns, &["checkout", "-q", "main"]);
    }
    // Nothing is inside the patterns, so nothing is written whatever the target.
    compare("excludes-all-hard", EXCLUDES_ALL, &["reset", "--hard", "HEAD~1"]);
    compare("excludes-all-checkout", EXCLUDES_ALL, &["checkout", "-q", "other"]);
}

#[test]
fn a_cone_mode_can_read_warns_nothing() {
    compare("cone-hard", "/*\n!/*/\n/a/\n", &["reset", "--hard", "HEAD~1"]);
    compare("cone-checkout", "/*\n!/*/\n/a/\n", &["checkout", "-q", "other"]);
}
