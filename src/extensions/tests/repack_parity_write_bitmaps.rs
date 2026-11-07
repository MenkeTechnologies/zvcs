//! How `repack` resolves `write_bitmaps` and what it does with it.
//!
//! * `repack.writeBitmaps` / `pack.writeBitmaps` seed the same variable, the
//!   last one in configuration order winning, so the config alone makes an
//!   incremental repack die with `incremental_bitmap_conflict_error`
//!   (builtin/repack.c:77-81, 416-418). zvcs only died for an explicit `-b`.
//! * `-l` in a repository with alternates warns `disabling bitmap writing, as
//!   some objects are not being packed` and writes none (:420-431).
//! * `--write-midx -b` writes the MIDX through `multi-pack-index write --bitmap
//!   --stdin-packs` before `-d` runs, so the run leaves a
//!   `multi-pack-index-<hash>.bitmap`, and a bitmap that cannot be built is the
//!   child's 255, after which nothing is deleted or pruned (:725-742). zvcs
//!   wrote a MIDX with no bitmap at all and pruned regardless.
//! * `-F` is `pack-objects --no-reuse-object`, which turns delta reuse off too
//!   (builtin/pack-objects.c:5370-5371); zvcs ignored it.
//! * A delta kept from an existing pack is written with its delta family, not
//!   among the trees (builtin/pack-objects.c:2351, 961-986).
//!
//! Stock git is the oracle (`support/stock_git.rs`).

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn git(bin: &str, dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir.parent().unwrap())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
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
    let root = dir.parent().unwrap().to_string_lossy().into_owned();
    let clean = |b: &[u8]| String::from_utf8_lossy(b).replace(&root, "<root>");
    (out.status.code().expect("no signal"), clean(&out.stdout), clean(&out.stderr))
}

struct World {
    root: PathBuf,
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl World {
    fn repo(&self) -> PathBuf {
        self.root.join("repo")
    }
}

/// `repo` with three commits, packed two and one with `t1` on the second when
/// `packs` says so,
/// and — with `alternate` — borrowing its first two commits from `base`.
fn world(stock: &str, label: &str, packs: bool, alternate: bool) -> World {
    let root = std::env::temp_dir().join(format!("zvcs-repack-bitmaps-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let w = World { root };
    let run = |dir: &Path, args: &[&str]| {
        let out = git(stock, dir, args);
        assert_eq!(out.0, 0, "{args:?}: {out:?}");
    };
    let commit = |dir: &Path, n: u32| {
        let body: String = (1..=n * 40).map(|i| format!("line {i}\n")).collect();
        std::fs::write(dir.join("f"), body).unwrap();
        run(dir, &["add", "f"]);
        run(dir, &["commit", "-q", "-m", &format!("c{n}")]);
        // Two commits per pack, so the first pack holds a delta of its own.
        if packs && n != 1 {
            run(dir, &["repack", "-q", "-d"]);
        }
    };
    let repo = w.repo();
    if alternate {
        let base = w.root.join("base");
        std::fs::create_dir_all(&base).unwrap();
        run(&base, &["init", "-q", "-b", "main"]);
        commit(&base, 1);
        commit(&base, 2);
        run(&w.root, &["clone", "-q", "--shared", "base", "repo"]);
    } else {
        std::fs::create_dir_all(&repo).unwrap();
        run(&repo, &["init", "-q", "-b", "main"]);
        commit(&repo, 1);
        commit(&repo, 2);
    }
    if packs {
        // A tag tip is what makes `compute_write_order()` regroup by type.
        run(&repo, &["tag", "-a", "-m", "t", "t1", "HEAD~1"]);
    }
    commit(&repo, 3);
    w
}

/// The files of `objects/pack`, and the number of loose objects left.
fn pack_dir(dir: &Path) -> (Vec<String>, usize) {
    let mut names: Vec<String> = std::fs::read_dir(dir.join(".git/objects/pack"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let loose = std::fs::read_dir(dir.join(".git/objects"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().len() == 2)
        .map(|e| std::fs::read_dir(e.path()).unwrap().count())
        .sum();
    (names, loose)
}

/// Run `args` in a fresh pair of worlds and assert exit, both streams and the
/// pack directory agree.
#[track_caller]
fn same(label: &str, packs: bool, alternate: bool, args: &[&str]) -> (i32, String) {
    let Some(stock) = stock_git() else { return (0, String::new()) };
    let s = world(stock, &format!("{label}-stock"), packs, alternate);
    let z = world(stock, &format!("{label}-zvcs"), packs, alternate);
    let want = git(stock, &s.repo(), args);
    let got = git(ZVCS, &z.repo(), args);
    assert_eq!(got, want, "{args:?}: left is zvcs, right is stock");
    assert_eq!(pack_dir(&z.repo()), pack_dir(&s.repo()), "{args:?}: objects/pack");
    (want.0, want.2)
}

#[test]
fn configured_bitmaps_make_an_incremental_repack_die() {
    let (code, stderr) = same("cfg-incremental", false, false, &["-c", "repack.writeBitmaps=true", "repack", "-d"]);
    if stock_git().is_some() {
        assert_eq!(code, 128);
        assert!(stderr.starts_with("fatal: Incremental repacks are incompatible"), "{stderr}");
    }
    same("cfg-incremental-old", false, false, &["-c", "pack.writeBitmaps=true", "repack", "-d"]);
}

#[test]
fn the_last_bitmap_key_in_config_order_wins() {
    same(
        "cfg-last-off",
        false,
        false,
        &["-c", "repack.writeBitmaps=true", "-c", "pack.writeBitmaps=false", "repack", "-q", "-d"],
    );
    same(
        "cfg-last-on",
        false,
        false,
        &["-c", "pack.writeBitmaps=false", "-c", "repack.writeBitmaps=true", "repack", "-q", "-d"],
    );
    same("cfg-bogus", false, false, &["-c", "repack.writeBitmaps=bogus", "repack", "-h"]);
}

#[test]
fn local_repack_over_alternates_disables_bitmaps() {
    let (_, stderr) = same("local-b", false, true, &["repack", "-q", "-a", "-d", "-l", "-b"]);
    if stock_git().is_some() {
        assert_eq!(stderr, "warning: disabling bitmap writing, as some objects are not being packed\n");
    }
    // Unasked, a MIDX write keeps `write_bitmaps` at -1, which is still true.
    same("local-midx", false, true, &["repack", "-q", "-a", "-d", "-l", "--write-midx"]);
    same("local-bare", false, true, &["-c", "core.bare=true", "repack", "-q", "-a", "-d", "-l"]);
}

#[test]
fn write_midx_with_bitmap_writes_the_midx_bitmap() {
    same("midx-b", false, false, &["repack", "-q", "-a", "-d", "-b", "--write-midx"]);
    same("midx-b-incremental", false, false, &["repack", "-q", "-d", "-b", "--write-midx"]);
    same("midx-cfg", false, false, &["-c", "repack.writeBitmaps=true", "repack", "-q", "-a", "-d", "--write-midx"]);
}

#[test]
fn a_midx_bitmap_without_closure_stops_before_d() {
    let (code, _) = same("midx-b-alt", false, true, &["repack", "-q", "--geometric=2", "-d", "-b", "--write-midx"]);
    if stock_git().is_some() {
        assert_eq!(code, 255);
    }
}

#[test]
fn big_f_turns_delta_reuse_off() {
    same("big-f", true, false, &["repack", "-q", "-a", "-d", "-F"]);
    same("big-f-small-f", true, false, &["repack", "-q", "-a", "-d", "-f", "-F"]);
}

/// A delta `check_object()` keeps from an existing pack has the in-pack delta
/// type as its `oe_type()` (builtin/pack-objects.c:2351), so
/// `compute_layer_order()` writes it with its family rather than among the
/// trees or commits (:961-986). zvcs grouped it by its object type, which gave
/// the pack different bytes and a different name.
#[test]
fn kept_deltas_are_written_with_their_family() {
    same("kept-delta", true, false, &["repack", "-q", "-a", "-d"]);
    same("kept-delta-midx", true, false, &["repack", "-q", "-a", "-d", "-b", "--write-midx"]);
}
