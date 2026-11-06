//! `for-each-ref --start-after=<marker>` lists what the ref iterator yields
//! once `start_ref_iterator_after()` (ref-filter.c:2817-2829) has seeked it to
//! `<marker>\001`.
//!
//! The reftable backend and the packed-refs file seek to exactly that name. The
//! files backend's loose refs go through `cache_ref_iterator_seek()`
//! (refs/ref-cache.c:471-551), which at a level where every entry sorts before
//! the seek path clamps to the last entry and, when that is a directory, yields
//! all of it: `zzz` lists every loose ref, `refs/zzz` lists none
//! (`refs/worktree/` is always the last entry of `refs/`), and
//! `refs/heads/nested/zzz` lists `refs/heads/nested/two/deep`. zvcs compared
//! names with the marker only when the marker started with `refs/`, so `zzz`
//! listed everything under reftable too and the loose-ref cases differed.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

/// Loose `main`, `zeta`, `nested/one`, `nested/two/deep`, tags `v1`, `v2`, a
/// custom ref; `pack/a` and `pack/b` only in packed-refs (files backend).
fn fixture(stock: &str, root: &Path, format: &str) -> PathBuf {
    let dir = root.join(format);
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let g = |args: &[&str]| {
        let (_, err, code) = run(stock, &dir, &home, args);
        assert_eq!(code, 0, "{args:?}: {err}");
    };
    std::fs::create_dir_all(&dir).unwrap();
    g(&["init", "-q", "-b", "main", "--ref-format", format, "."]);
    g(&["commit", "-q", "--allow-empty", "-m", "one"]);
    g(&["branch", "pack/a"]);
    g(&["branch", "pack/b"]);
    g(&["pack-refs", "--all"]);
    g(&["commit", "-q", "--allow-empty", "-m", "two"]);
    for b in ["zeta", "nested/one", "nested/two/deep"] {
        g(&["branch", b]);
    }
    g(&["tag", "v1"]);
    g(&["tag", "v2", "HEAD~1"]);
    g(&["update-ref", "refs/custom/x", "HEAD"]);
    dir
}

const MARKERS: [&str; 14] = [
    "zzz",
    "a",
    "refs",
    "refs/zzz",
    "refs/s",
    "refs/u",
    "refs/heads/nested/zzz",
    "refs/heads/nested/zzz/q",
    "refs/heads/nested/two/zz",
    "refs/heads/m/x",
    "refs/heads/main",
    "refs/heads/pack/a",
    "refs/tags/zzz",
    "HEAD",
];

fn compare(format: &str) {
    let Some(stock) = stock_git::stock_git() else {
        return;
    };
    let root = std::env::temp_dir().join(format!("zvcs-fer-start-after-{format}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = fixture(stock, &root, format);
    let home = root.join("home");
    for marker in MARKERS {
        for extra in [&[][..], &["--include-root-refs"][..]] {
            let opt = format!("--start-after={marker}");
            let mut args = vec!["for-each-ref", "--format=%(refname)", opt.as_str()];
            args.extend_from_slice(extra);
            assert_eq!(
                run(BIN, &dir, &home, &args),
                run(stock, &dir, &home, &args),
                "{format} {args:?}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn start_after_seeks_the_loose_ref_cache_as_git_does() {
    compare("files");
}

#[test]
fn start_after_seeks_reftable_to_the_marker() {
    compare("reftable");
}
