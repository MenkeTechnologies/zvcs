//! `cherry-pick -n` writes the index as a tree before it merges anything.
//!
//! `do_pick_commit()` takes its *ours* side from `write_index_as_tree()` under `--no-commit`
//! (sequencer.c:2314), which re-reads the index file and, its cache-tree not being fully valid,
//! writes the trees `cache_tree_update()` builds and the index with its TREE extension
//! (cache-tree.c:810-844). On an unborn branch with an empty index that is the empty tree, left
//! dangling in the odb, and an index file that keeps the extension even when the resolve strategy
//! fails right after. zvcs computed the tree without writing the index.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
        .stdin(Stdio::null())
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

/// `main` with two commits touching `f` and `d/g`, checked out on the orphan `new` whose index
/// and worktree are then emptied, so the index left behind carries an invalidated TREE.
fn prepare(stock: &str, r: &Path) {
    let ok = |args: &[&str]| assert_eq!(run(stock, r, args).2, 0, "{args:?}");
    ok(&["init", "-q", "-b", "main", "."]);
    std::fs::create_dir_all(r.join("d")).unwrap();
    std::fs::write(r.join("f"), "1\n").unwrap();
    std::fs::write(r.join("d/g"), "2\n").unwrap();
    ok(&["add", "."]);
    ok(&["-c", "maintenance.auto=false", "commit", "-q", "-m", "one"]);
    std::fs::write(r.join("f"), "3\n").unwrap();
    ok(&["-c", "maintenance.auto=false", "commit", "-q", "-am", "two"]);
    ok(&["checkout", "-q", "--orphan", "new"]);
    ok(&["rm", "-rqf", "."]);
}

fn compare(tag: &str, args: &[&str]) {
    let Some(stock) = stock_git::stock_git() else {
        return;
    };
    let base = std::env::temp_dir().join(format!("zvcs-cp-no-commit-tree-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let _twin = Twin { root: base.clone() };
    let mut outputs = Vec::new();
    for (side, bin) in [("s", stock), ("z", BIN)] {
        let r = base.join(side);
        std::fs::create_dir_all(&r).unwrap();
        prepare(stock, &r);
        let out = run(bin, &r, args);
        let mut state = run(stock, &r, &["ls-files", "-s"]).0;
        state.push_str(&run(stock, &r, &["fsck"]).0);
        // An index with no entries holds no stat data, so the whole file, TREE extension and
        // checksum included, is the same bytes on both sides.
        let index = std::fs::read(r.join(".git/index")).unwrap();
        outputs.push((out, state, index));
    }
    assert_eq!(outputs[1], outputs[0], "{tag}: {args:?}");
}

#[test]
fn no_commit_onto_an_empty_unborn_index_writes_its_tree_first() {
    compare("resolve", &["cherry-pick", "-n", "--strategy=resolve", "main"]);
    compare("resolve-root", &["cherry-pick", "-n", "--strategy=resolve", "main~1"]);
}
