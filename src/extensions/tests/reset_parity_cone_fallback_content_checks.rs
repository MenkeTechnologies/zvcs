//! The cone-loading warnings of an `info/sparse-checkout` cone mode cannot read, for the
//! content comparisons that read attributes from an index.
//!
//! `init_sparse_checkout_patterns()` (dir.c:1557-1574) loads the file into each
//! `index_state` once, and `add_pattern_to_hashsets()` (dir.c:798-936) warns at each load.
//! Besides `unpack_trees()`' own list, an index loads it when `read_attr_from_index()` asks
//! `path_in_cone_mode_sparse_checkout()` of it (attr.c:821): `ce_compare_data()` of a racily
//! clean entry (`verify_uptodate()`, `oneway_merge()`, `refresh_index()`, `run_diff_files()`),
//! the stat-dirty pair `diffcore_skip_stat_unmatch()` reads, and each file `checkout_paths()`
//! writes, under `GIT_ATTR_CHECKIN`, whose reads go to the index only where the worktree
//! has no `.gitattributes` (attr.c:857-865).
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
const PATTERNS: &str = "/a/*\n";

#[derive(Clone, Copy, PartialEq)]
enum Stat {
    Racy,
    Dirty,
}

fn date_2020(path: &Path) {
    let past = SystemTime::UNIX_EPOCH + Duration::from_secs(1_577_836_800);
    std::fs::File::options().write(true).open(path).unwrap().set_modified(past).unwrap();
}

/// `main` (two commits over `top`, `a/x`, `a/b/y`, `c/z` and a `.gitattributes` in each
/// directory of `attrs`) and `other` at the first, in a cone-mode sparse checkout whose
/// pattern file cone mode cannot read.
fn prepare(stock: &str, r: &Path, stat: Stat, attrs: &[&str]) {
    std::fs::create_dir_all(r.join("a/b")).unwrap();
    std::fs::create_dir_all(r.join("c")).unwrap();
    for dir in attrs {
        std::fs::write(r.join(dir).join(".gitattributes"), "*.zz text\n").unwrap();
    }
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
    for path in FILES {
        date_2020(&r.join(path));
    }
    for dir in attrs {
        date_2020(&r.join(dir).join(".gitattributes"));
    }
    if stat == Stat::Racy {
        ok(&["update-index", "--refresh"]);
        date_2020(&r.join(".git/index"));
    }
    ok(&["config", "core.sparseCheckout", "true"]);
    ok(&["config", "core.sparseCheckoutCone", "true"]);
    std::fs::write(r.join(".git/info/sparse-checkout"), PATTERNS).unwrap();
}

/// Run `args` on twin fixtures, then stock `ls-files -t`, and compare both runs and the
/// files on disk afterwards.
fn compare(tag: &str, stat: Stat, attrs: &[&str], args: &[&str]) {
    let Some(stock) = stock_git::stock_git() else {
        return;
    };
    let base = std::env::temp_dir().join(format!("zvcs-cone-content-checks-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let _twin = Twin { root: base.clone() };
    let mut outputs = Vec::new();
    for (side, bin) in [("s", stock), ("z", BIN)] {
        let r = base.join(side);
        std::fs::create_dir_all(&r).unwrap();
        prepare(stock, &r, stat, attrs);
        let mut out = vec![run(bin, &r, args), run(stock, &r, &["ls-files", "-t"])];
        for path in FILES {
            out.push((path.to_owned(), std::fs::read_to_string(r.join(path)).unwrap_or_default(), 0));
        }
        outputs.push(out);
    }
    assert_eq!(outputs[1], outputs[0], "{tag}: {args:?}");
}

#[test]
fn an_unpack_hashing_a_racily_clean_source_entry_loads_the_index_read() {
    let racy = Stat::Racy;
    // `oneway_merge()` under `o->reset` (unpack-trees.c:3036-3042).
    compare("hard", racy, &[], &["reset", "--hard"]);
    // `verify_uptodate()` of each entry the two-way merge replaces, then for `--keep` the
    // second, MIXED `unpack_trees()` whose fresh result the index write smudges from.
    compare("keep", racy, &[], &["reset", "--keep", "other"]);
    // `verify_uptodate_sparse()` of the entries leaving the worktree (unpack-trees.c:2282-2286).
    compare("read-tree-same", racy, &[], &["read-tree", "-m", "-u", "HEAD"]);
    // `refresh_index()` before the two-way switch (builtin/checkout.c:883).
    compare("checkout", racy, &[], &["checkout", "-q", "other"]);
}

#[test]
fn a_gitattributes_in_the_worktree_answers_before_the_index() {
    // `top` and `c/z` leave the worktree and are compared, and their directories have a
    // `.gitattributes` on disk; only the result's smudge reads `a/` from an index.
    compare("attrs-read-tree", Stat::Racy, &[".", "c"], &["read-tree", "-m", "-u", "HEAD"]);
}

#[test]
fn a_path_checkout_and_the_refreshes_load_the_index_they_read() {
    let racy = Stat::Racy;
    compare("restore", racy, &[], &["restore", "-s", "other", "a/x"]);
    compare("mixed", racy, &[], &["reset", "-q", "HEAD~1"]);
    compare("status", racy, &[], &["status", "--porcelain"]);
    compare("diff", racy, &[], &["diff", "--stat"]);
    // Without a refresh `checkout_paths()`' write is the first read from the index.
    compare("restore-dirty", Stat::Dirty, &[], &["restore", "-s", "other", "a/x"]);
    // A stat-dirty pair is read once by `diffcore_skip_stat_unmatch()` and once more by
    // `refresh_index_quietly()` on the index it reads afresh (builtin/diff.c:237-250).
    compare("diff-dirty", Stat::Dirty, &[], &["diff", "--stat"]);
}

#[test]
fn the_unpack_warns_before_its_merge_refuses() {
    // `populate_from_existing_patterns()` runs ahead of `traverse_trees()`
    // (unpack-trees.c:1931-1936), so its pair precedes the refusal.
    compare("keep-refused", Stat::Dirty, &[], &["reset", "--keep", "other"]);
}
