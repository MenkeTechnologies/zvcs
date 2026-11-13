//! `git write-tree --prefix=<p>` looks `<p>` up in the cache-tree it has just
//! brought up to date, through `cache_tree_find()` (cache-tree.c:733-756,
//! :772-776), not in the tree objects it wrote:
//!
//! ```c
//! while (*path) {
//!         slash = strchrnul(path, '/');
//!         sub = find_subtree(it, path, slash - path, 0);
//!         if (!sub)
//!                 return NULL;
//!         it = sub->cache_tree;
//!         path = slash;
//!         while (*path == '/')
//!                 path++;
//! }
//! ```
//!
//! So a leading `/` names an empty component and is `prefix / not found`, and a
//! directory a sparse index keeps collapsed has no node below it: `out/deep/` is
//! not found there although the tree object holds it. zvcs walked the root tree
//! object, which found both.
//!
//! `write_index_as_tree()` takes the index lock with `LOCK_DIE_ON_ERROR` before it
//! reads anything (cache-tree.c:816), so an index whose lock cannot be created is
//! `unable_to_lock_message()` at 128 — `GIT_INDEX_FILE=/dev/null` cannot have a
//! `/dev/null.lock`. zvcs read the index first and failed on that with its own
//! sentence.
//!
//! `--end-of-options` ends the options as `--` does (parse-options.c:1119-1124).
//!
//! Expectations come from stock git (`support/stock_git.rs`) in an identical
//! repository.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

/// Run stock git in `side`'s repository, so both sides are set up byte for byte
/// alike.
fn stock_in(stock: &str, side: &twin_repo::Side, args: &[&str]) {
    let out = std::process::Command::new(stock)
        .args(args)
        .current_dir(side.repo())
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &side.root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .unwrap();
    assert!(out.status.success(), "{args:?}: {out:?}");
}

#[test]
fn prefix_is_found_in_the_cache_tree() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("write-tree-prefix", stock);
    for side in [&s, &z] {
        side.write("in/i", "i\n");
        side.write("out/deep/d", "d\n");
        side.write("out/o", "o\n");
        stock_in(stock, side, &["add", "."]);
        stock_in(stock, side, &["-c", "maintenance.auto=false", "commit", "-q", "-m", "dirs"]);
        stock_in(stock, side, &["sparse-checkout", "set", "--cone", "--sparse-index", "in"]);
    }
    for args in [
        &["write-tree", "--prefix=out/"][..],
        &["write-tree", "--prefix=out/deep/"],
        &["write-tree", "--prefix=/"],
        &["write-tree", "--prefix=in//"],
        &["write-tree", "--end-of-options", "--prefix=out/"],
    ] {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
    }
    assert_eq!(z.git_env(&[("GIT_INDEX_FILE", "/dev/null")], &["write-tree"]),
               s.git_env(&[("GIT_INDEX_FILE", "/dev/null")], &["write-tree"]));
}
