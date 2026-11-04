//! `git stash push` against stock git for the objects the index tree leaves behind:
//! `do_create_stash()` takes `i_tree` from `write_index_as_tree()`, whose
//! `cache_tree_update()` writes only the nodes it rebuilds (cache-tree.c:497-503). After
//! `read-tree --empty` the cache-tree already records the empty tree, so stock never stores
//! it and `fsck` reports `missing tree 4b825dc…`; after `rm --cached` the root is rebuilt
//! and the empty tree is written.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use std::path::Path;
use std::process::Command;

fn stock_view(stock: &str, repo: &Path, args: &[&str]) -> String {
    let out = Command::new(stock)
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
}

#[test]
fn stash_names_the_recorded_root_without_writing_it() {
    let Some(stock) = stock_git::stock_git() else { return };
    for (n, (empty_index, args)) in [
        (&["read-tree", "--empty"][..], &["stash", "push"][..]),
        (&["read-tree", "--empty"], &["stash", "push", "-k"]),
        (&["rm", "-q", "--cached", "a"], &["stash", "push"]),
    ]
    .into_iter()
    .enumerate()
    {
        let (s, z) = twin_repo::pair(&format!("stash-cache-tree-root-{n}"), stock);
        for side in [&s, &z] {
            side.git(empty_index);
        }
        assert_eq!(z.git(args), s.git(args), "{empty_index:?} {args:?}");
        for view in [&["fsck"][..], &["status", "--porcelain"], &["log", "--format=%T %s", "-3", "refs/stash"]] {
            assert_eq!(
                stock_view(stock, &z.repo(), view),
                stock_view(stock, &s.repo(), view),
                "{view:?} after {empty_index:?} {args:?}"
            );
        }
    }
}
