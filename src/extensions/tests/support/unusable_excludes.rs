//! The [`twin_repo`](super::twin_repo) pair with an unusable `core.excludesFile`,
//! for the commands whose C path reaches `unpack_trees()` and so
//! `setup_standard_excludes()`, which dies with `cannot use <path> as an exclude
//! file` (unpack-trees.c:1922-1926, dir.c:1013).
//!
//! Included with
//!
//! ```ignore
//! #[path = "support/stock_git.rs"]
//! mod stock_git;
//! #[path = "support/twin_repo.rs"]
//! mod twin_repo;
//! #[path = "support/unusable_excludes.rs"]
//! mod unusable_excludes;
//! ```

#![allow(dead_code)]

use super::stock_git;
use super::twin_repo::{pair, Side};

/// The two worlds with `a` dirty and `core.excludesFile` naming the untracked
/// directory `d`, or `None` without a stock git 2.56.
pub fn broken(label: &str) -> Option<(Side, Side)> {
    let stock = stock_git::stock_git_at_least((2, 56, 0))?;
    let (s, z) = pair(label, stock);
    for side in [&s, &z] {
        side.write("d/x", "x\n");
        side.write("a", "dirty\n");
        side.git(&["config", "core.excludesFile", "d"]);
    }
    Some((s, z))
}

/// `args` run by stock git (`s.bin`) in `side`'s repository: its stdout.
fn probe(s: &Side, side: &Side, args: &[&str]) -> String {
    let out = std::process::Command::new(&s.bin)
        .args(args)
        .current_dir(side.repo())
        .env_clear()
        .env("HOME", &side.root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Run `args` in both worlds and compare what they print, then what they left:
/// the refs, `ORIG_HEAD`, the status, the reflogs and the index entries, all
/// read by stock git. Answers the exit status.
#[track_caller]
pub fn same(s: &Side, z: &Side, args: &[&str]) -> i32 {
    let (so, zo) = (s.git(args), z.git(args));
    assert_eq!(zo, so, "git {args:?}: left is zvcs, right is stock");
    for probe_args in [
        &["for-each-ref"][..],
        &["rev-parse", "-q", "--verify", "ORIG_HEAD"],
        &["status", "--porcelain"],
        &["reflog", "list"],
        &["ls-files", "-s"],
    ] {
        assert_eq!(probe(s, z, probe_args), probe(s, s, probe_args), "after git {args:?}: {probe_args:?}");
    }
    so.code
}
