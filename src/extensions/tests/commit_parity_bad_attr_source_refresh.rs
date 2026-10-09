//! `git commit` (as-is branch) refreshes the index before it looks at anything, and the
//! refresh's content compare of a racily clean entry is the first attribute lookup.
//!
//! ```c
//! repo_hold_locked_index(the_repository, &index_lock, LOCK_DIE_ON_ERROR);
//! refresh_cache_or_die(refresh_flags);
//! ```
//! (`prepare_index()`, builtin/commit.c:482-484.) A `GIT_ATTR_SOURCE` naming no tree-ish
//! therefore dies in `compute_default_attr_source()` with `fatal: bad --attr-source or
//! GIT_ATTR_SOURCE` and exit 128, whether or not there is anything to commit. zvcs reported
//! `nothing to commit` and exit 1. A dry run takes the same branch.
//! Expectations come from stock git (`support/stock_git.rs`) in an identical repository.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use std::time::{Duration, SystemTime};

fn stale_stat(side: &twin_repo::Side) {
    let file = std::fs::File::options().write(true).open(side.repo().join("a")).unwrap();
    file.set_modified(SystemTime::now() + Duration::from_secs(3600)).unwrap();
}

#[test]
fn bad_attr_source_dies_in_the_commit_refresh() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("commit-attr-source", stock);
    for side in [&s, &z] {
        stale_stat(side);
    }
    let cases: [(&str, &[&str]); 4] = [
        ("", &["commit", "-m", "x"]),
        ("", &["commit", "--branch", "--no-all", "-m", "x"]),
        ("", &["commit", "--dry-run"]),
        ("refs/heads/nope", &["commit", "-m", "x"]),
    ];
    for (source, args) in cases {
        let env = [("GIT_ATTR_SOURCE", source)];
        let (want, got) = (s.git_env(&env, args), z.git_env(&env, args));
        assert_eq!(want.code, if source.is_empty() || source.contains("nope") { 128 } else { 1 }, "{args:?}: {want:?}");
        assert_eq!(got, want, "{source:?} {args:?}");
    }
}

#[test]
fn a_resolvable_attr_source_is_not_fatal() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("commit-attr-source-ok", stock);
    for side in [&s, &z] {
        stale_stat(side);
    }
    let env = [("GIT_ATTR_SOURCE", "HEAD")];
    let args = ["commit", "-m", "x"];
    assert_eq!(z.git_env(&env, &args), s.git_env(&env, &args));
}
