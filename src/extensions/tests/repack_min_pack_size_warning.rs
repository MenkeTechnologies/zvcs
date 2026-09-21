//! `git repack --max-pack-size=<n>` below git's 1 MiB floor: how many times the
//! warning appears.
//!
//! The warning is not repack's. `cmd_repack()` forwards `--max-pack-size` to a
//! `pack-objects` child and the child runs the check:
//!
//! ```c
//! if (!pack_to_stdout && !pack_size_limit)
//!         pack_size_limit = pack_size_limit_cfg;
//! [...]
//! if (pack_size_limit && pack_size_limit < 1024*1024) {
//!         warning(_("minimum pack size limit is 1 MiB"));
//!         pack_size_limit = 1024*1024;
//! }
//! ```
//!
//! (builtin/pack-objects.c:5291-5298.) So the count is structural: one per child
//! git spawns, not one per `repack` run. A `--cruft` run spawns a second child
//! for the cruft pack (builtin/repack.c:480-506), `-d --expire-to=<dir>` a third
//! (:510-544), and `--filter` one more for the filtered pack (:547-558). The
//! cruft child carries `cruft_po_args.max_pack_size`, which is
//! `--max-cruft-size` and only falls back to `--max-pack-size` when that is
//! unset (:496-497) — so `--max-cruft-size=1` alone warns exactly once, from the
//! cruft child, while the main child is left unlimited.
//!
//! Every count below was measured against git 2.55.0 on the same fixture.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");
const WARNING: &str = "warning: minimum pack size limit is 1 MiB";

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .output()
        .expect("run binary")
}

fn ok(dir: &Path, args: &[&str]) -> Output {
    let out = run(dir, args);
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    out
}

/// How many times the floor warning appears on stderr.
fn warnings(dir: &Path, args: &[&str]) -> usize {
    let out = ok(dir, args);
    String::from_utf8_lossy(&out.stderr).lines().filter(|l| *l == WARNING).count()
}

/// One reachable commit and one object nothing names, so a `--cruft` run has
/// something to put in its second pack.
fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-minpack-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir fixture");
    ok(&dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("a.txt"), "a\n").expect("write");
    ok(&dir, &["add", "a.txt"]);
    ok(&dir, &["commit", "-qm", "a"]);
    std::fs::write(dir.join("loose.txt"), "unreferenced\n").expect("write");
    ok(&dir, &["hash-object", "-w", "loose.txt"]);
    dir
}

#[test]
fn one_pack_objects_child_warns_once() {
    let dir = fixture("one");
    assert_eq!(warnings(&dir, &["repack", "--max-pack-size=1"]), 1);
    assert_eq!(warnings(&dir, &["repack", "-ad", "--max-pack-size=1"]), 1);
    // At or above the floor there is nothing to say.
    assert_eq!(warnings(&dir, &["repack", "-ad", "--max-pack-size=2097152"]), 0);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_cruft_child_warns_on_top_of_the_main_one() {
    let dir = fixture("cruft");
    assert_eq!(
        warnings(&dir, &["repack", "-d", "--cruft", "--max-pack-size=1"]),
        2,
        "the main pack-objects and the cruft one both get the limit"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// `--max-cruft-size` is `cruft_po_args.max_pack_size` and nothing else's, so it
/// reaches the cruft child alone — the main child is still unlimited and silent.
#[test]
fn max_cruft_size_warns_only_from_the_cruft_child() {
    let dir = fixture("cruftsize");
    assert_eq!(warnings(&dir, &["repack", "-d", "--cruft", "--max-cruft-size=1"]), 1);
    // With both set below the floor each child has its own sub-1 MiB limit.
    assert_eq!(
        warnings(&dir, &["repack", "-d", "--cruft", "--max-cruft-size=1", "--max-pack-size=2"]),
        2
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// `--expire-to` with `-d` runs `write_cruft_pack()` a second time, so the cruft
/// child's warning appears twice on top of the main pack's.
#[test]
fn expire_to_adds_a_third_child() {
    let dir = fixture("expireto");
    let limbo = dir.join("limbo");
    std::fs::create_dir_all(&limbo).expect("mkdir limbo");
    let limbo = limbo.to_str().expect("utf-8 path").to_string();
    assert_eq!(
        warnings(
            &dir,
            &["repack", "-d", "--cruft", &format!("--expire-to={limbo}"), "--max-pack-size=1"]
        ),
        3
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// `--filter` writes a second pack from `po_args`, so its child sees the same
/// limit the main one did.
#[test]
fn the_filtered_pack_child_warns_too() {
    let dir = fixture("filter");
    assert_eq!(
        warnings(&dir, &["repack", "-ad", "--filter=blob:none", "--max-pack-size=1"]),
        2
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// repack forwards no `--max-pack-size` when it has none, and the child then
/// reads `pack.packSizeLimit` for itself — so the config reaches every child the
/// option would have.
#[test]
fn pack_size_limit_config_reaches_each_child() {
    let dir = fixture("config");
    assert_eq!(warnings(&dir, &["-c", "pack.packSizeLimit=1", "repack", "-d", "--cruft"]), 2);
    std::fs::remove_dir_all(&dir).ok();
}
