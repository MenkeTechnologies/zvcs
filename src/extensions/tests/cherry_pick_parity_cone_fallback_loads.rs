//! The cone-loading warnings of an `info/sparse-checkout` cone mode cannot read, for a
//! `cherry-pick` in a sparse checkout.
//!
//! `add_pattern_to_hashsets()` (dir.c:798-936) warns each time an `index_state` loads the
//! file (dir.c:1557-1574). A pick loads it into: the index `read_and_refresh_cache()` refreshes
//! (sequencer.c:2612-2639) when an entry is hashed; merge-ort's `attr_index` when `ll_merge()`
//! reads attributes (merge-ort.c:2161-2163, merge-ll.c:429); the pattern list of the
//! `unpack_trees()` its `checkout()` runs, before any entry is looked at (unpack-trees.c:1931-1936);
//! the re-read source index when `verify_uptodate()` hashes a racily clean entry; and the result,
//! at the first file `check_updates()` writes or else at the summary's diffstat
//! (`reuse_worktree_file()`, diff.c:4409-4417). The sparse loops of that unpack also take the
//! paths outside the definition out of the worktree.
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
        .env_remove("VISUAL")
        .env_remove("EDITOR")
        .env("GIT_EDITOR", "true")
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

const FILES: [&str; 6] = ["top", "a/x", "a/b/y", "c/z", "a/m", "c/m"];

#[derive(Clone, Copy, PartialEq)]
enum Stat {
    Racy,
    Dirty,
}

fn date_2020(path: &Path) {
    let past = SystemTime::UNIX_EPOCH + Duration::from_secs(1_577_836_800);
    std::fs::File::options().write(true).open(path).unwrap().set_modified(past).unwrap();
}

/// `main` (`one` over `top`, `a/x`, `a/b/y`, `c/z`, `a/m`, `c/m`, then `two`); `pick`, a
/// commit on `main` changing `top` and `a/b/y`; `mpick` and `cpick`, each changing the last
/// line of `a/m` or `c/m` where `main` changed the first, so picking one content-merges. A
/// cone-mode sparse checkout whose pattern file cone mode cannot read.
fn prepare(stock: &str, r: &Path, stat: Stat) {
    std::fs::create_dir_all(r.join("a/b")).unwrap();
    std::fs::create_dir_all(r.join("c")).unwrap();
    let five = "1\n2\n3\n4\n5\n";
    for (path, body) in FILES.iter().zip(["1\n", "2\n", "3\n", "4\n", five, five]) {
        std::fs::write(r.join(path), body).unwrap();
    }
    let ok = |args: &[&str]| assert_eq!(run(stock, r, args).2, 0, "{args:?}");
    let commit = |msg: &str| ok(&["-c", "maintenance.auto=false", "commit", "-q", "-a", "-m", msg]);
    ok(&["init", "-q", "-b", "main", "."]);
    ok(&["add", "."]);
    commit("one");
    for (branch, path) in [("mpick", "a/m"), ("cpick", "c/m")] {
        ok(&["checkout", "-q", "-b", branch, "main"]);
        std::fs::write(r.join(path), "1\n2\n3\n4\nfive\n").unwrap();
        commit(branch);
    }
    ok(&["checkout", "-q", "main"]);
    for (path, body) in [("top", "11\n"), ("a/x", "22\n"), ("c/z", "44\n")] {
        std::fs::write(r.join(path), body).unwrap();
    }
    for path in ["a/m", "c/m"] {
        std::fs::write(r.join(path), "one\n2\n3\n4\n5\n").unwrap();
    }
    commit("two");
    ok(&["checkout", "-q", "-b", "pick"]);
    std::fs::write(r.join("top"), "111\n").unwrap();
    std::fs::write(r.join("a/b/y"), "33\n").unwrap();
    commit("three");
    ok(&["checkout", "-q", "main"]);
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

/// Run `args` on twin fixtures, after `setup` writes `(path, body)` pairs, then compare both
/// runs, stock's `ls-files -t` and the files on disk afterwards.
fn compare(tag: &str, stat: Stat, setup: &[(&str, &str)], args: &[&str]) {
    let Some(stock) = stock_git::stock_git() else {
        return;
    };
    let base = std::env::temp_dir().join(format!("zvcs-cp-cone-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let _twin = Twin { root: base.clone() };
    let mut outputs = Vec::new();
    for (side, bin) in [("s", stock), ("z", BIN)] {
        let r = base.join(side);
        std::fs::create_dir_all(&r).unwrap();
        prepare(stock, &r, stat);
        for (path, body) in setup {
            std::fs::write(r.join(path), body).unwrap();
        }
        let mut out = vec![run(bin, &r, args), run(stock, &r, &["ls-files", "-t"])];
        for path in FILES {
            out.push((path.to_owned(), std::fs::read_to_string(r.join(path)).unwrap_or_default(), 0));
        }
        outputs.push(out);
    }
    assert_eq!(outputs[1], outputs[0], "{tag}: {args:?}");
}

#[test]
fn a_pick_loads_the_refreshed_index_its_unpack_and_its_result() {
    // The refresh hashes the stat-dirty entries; the result loads at the first write, `a/b/y`.
    compare("dirty", Stat::Dirty, &[], &["cherry-pick", "pick"]);
    // The racily clean `top` the merge replaces is hashed again from the index re-read.
    compare("racy", Stat::Racy, &[], &["cherry-pick", "pick"]);
    // `checkout_fast_forward()` runs the same two-way unpack.
    compare("ff", Stat::Racy, &[], &["cherry-pick", "--ff", "pick"]);
}

#[test]
fn a_content_merge_loads_merge_orts_attr_index() {
    compare("merge-in-cone", Stat::Racy, &[], &["cherry-pick", "mpick"]);
    // `c/m` goes out of the worktree, so nothing is written and the summary's diffstat is
    // the result's first read.
    compare("merge-out-of-cone", Stat::Dirty, &[], &["cherry-pick", "cpick"]);
    compare("merge-no-commit", Stat::Dirty, &[], &["cherry-pick", "-n", "cpick"]);
}

#[test]
fn the_unpack_warns_before_the_merge_refuses() {
    compare("refused", Stat::Dirty, &[("top", "dirty\n")], &["cherry-pick", "pick"]);
    compare("ff-refused", Stat::Dirty, &[("top", "dirty\n")], &["cherry-pick", "--ff", "pick"]);
}
