//! `repack --filter=<spec> --write-midx` over an empty second pack.
//!
//! `--filter` splits the object set in two. The second pack holds what the
//! *existing* packs held and the new pack does not (`write_filtered_pack()` runs
//! `pack-objects --stdin-packs` over them with the new pack excluded by `^`), so
//! when the new pack already covers all of them it is written *empty*.
//! `--write-midx` then makes `cmd_repack()` call `write_midx_included_packs()`,
//! which names a `--preferred-pack`: the first of the packs the run just wrote,
//! `names` having been sorted. `write_midx_internal()` refuses a preferred pack
//! with no objects —
//!
//! ```text
//! error: cannot select preferred pack <objdir>/pack/pack-<hash>.pack with no objects
//! ```
//!
//! — and `goto cleanup`s with `result` still at its `-1` initializer, so the
//! `multi-pack-index` child exits 255 and `cmd_repack()` returns that untouched.
//!
//! The exit status is only half of it. That `goto cleanup` happens *after* the
//! new packs are installed and *before* anything is removed, so the failed run
//! must leave: both new packs, every pack `-d` would otherwise have deleted, no
//! `multi-pack-index`, no bitmap of either kind, and no refreshed
//! `objects/info/packs`. Writing a midx and exiting 0 — which is what this port
//! did before — silently disagrees with git about the repository's state, not
//! just about a number.
//!
//! Every expectation below was read off git 2.55.0 run over the same fixtures.
//! The empty pack's name is not spelled out anywhere here: it is found on disk by
//! its zero object count, so the assertions hold for any hash function.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Run the binary under test in `dir`, asserting success. Fixture only.
fn git(dir: &Path, home: &Path, args: &[&str]) {
    let out = run(dir, home, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Run the binary under test with an isolated, deterministic environment, so no
/// ambient `repack.*` or `pack.*` config can steer the run.
fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("ZVCS_HOME", home)
        .env("LC_ALL", "C")
        .output()
        .unwrap()
}

/// A repository in **two** packs whose every blob is still named by the index.
///
/// Both properties are load-bearing. Two packs give `-d` something real to
/// delete, so "the failed run deleted nothing" is an assertion rather than a
/// tautology. Every blob being in the index is what empties the second pack:
/// `blob:none` rejects the blobs, `cmd_repack()` unions the index objects back
/// in, and so the main pack ends up covering everything the old packs held.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-rpfmidx-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &home, &["init", "-q", "-b", "main"]);
    git(&repo, &home, &["config", "user.email", "alice@example.com"]);
    git(&repo, &home, &["config", "user.name", "Alice"]);
    for (n, name) in ["f", "g"].iter().enumerate() {
        std::fs::write(repo.join(name), format!("contents of {name}\n")).unwrap();
        git(&repo, &home, &["add", name]);
        git(&repo, &home, &["commit", "-q", "-m", &format!("c{n}")]);
        // `-d` without `-a` packs what is loose and leaves the earlier pack
        // alone, so the two commits land in two packs.
        git(&repo, &home, &["repack", "-d", "-q"]);
    }
    assert_eq!(pack_files(&repo).len(), 2, "fixture should hold two packs");
    (repo, home)
}

/// The `objects/pack` entries of `repo`, sorted.
fn pack_dir_entries(repo: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(repo.join(".git/objects/pack"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Just the `.pack` files, sorted.
fn pack_files(repo: &Path) -> Vec<String> {
    pack_dir_entries(repo).into_iter().filter(|n| n.ends_with(".pack")).collect()
}

/// The `pack-<hash>` stem of the pack in `repo` that holds no objects, read from
/// the object count in the v2 pack header rather than from a known name.
fn empty_pack_stem(repo: &Path) -> String {
    for name in pack_files(repo) {
        let bytes = std::fs::read(repo.join(".git/objects/pack").join(&name)).unwrap();
        let count = u32::from_be_bytes(bytes[8..12].try_into().unwrap());
        if count == 0 {
            return name.trim_end_matches(".pack").to_string();
        }
    }
    panic!("no empty pack in {:?}", pack_dir_entries(repo));
}

/// The failure itself: git's message, git's exit status, and a repository whose
/// only change is the two packs the run installed before it failed.
#[test]
fn empty_filtered_pack_refuses_to_be_preferred() {
    let (repo, home) = fixture("refuse");
    // The fixture's own repacks left one behind; its *absence* afterwards is what
    // shows the failed run never reached `update_server_info()`.
    std::fs::remove_file(repo.join(".git/objects/info/packs")).unwrap();

    let out = run(&repo, &home, &["repack", "-a", "-b", "--write-midx", "--filter=blob:none"]);

    assert_eq!(out.status.code(), Some(255), "exit status");
    let stem = empty_pack_stem(&repo);
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        format!("error: cannot select preferred pack .git/objects/pack/{stem}.pack with no objects\n"),
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "", "nothing on stdout");

    // The `goto cleanup` fires before a single byte of the index is written, so
    // neither the index nor its lock survives the failure.
    let after = pack_dir_entries(&repo);
    assert!(!after.iter().any(|n| n.starts_with("multi-pack-index")), "left a midx: {after:?}");
    assert!(!after.iter().any(|n| n.ends_with(".bitmap")), "left a bitmap: {after:?}");
    assert!(!after.iter().any(|n| n.ends_with(".lock")), "left a lock: {after:?}");

    // The empty pack is installed all the same: the failure is downstream of the
    // pack write, not a refusal to perform it.
    for ext in ["pack", "idx"] {
        assert!(after.contains(&format!("{stem}.{ext}")), "empty pack .{ext} missing: {after:?}");
    }

    // `update_server_info()` runs after the midx write, so the failure skips it.
    assert!(
        !repo.join(".git/objects/info/packs").exists(),
        "objects/info/packs refreshed by a run that failed"
    );
}

/// `-d`'s deletions happen *after* the midx write in `cmd_repack()`, so a run
/// that fails there leaves the superseded packs in place. This is the difference
/// between a failed repack and a repack that dropped two packs and then failed.
#[test]
fn failed_midx_write_deletes_no_superseded_pack() {
    let (repo, home) = fixture("keep");
    let before = pack_dir_entries(&repo);

    let out =
        run(&repo, &home, &["repack", "-a", "-d", "-b", "--write-midx", "--filter=blob:none"]);
    assert_eq!(out.status.code(), Some(255), "exit status");

    let after = pack_dir_entries(&repo);
    for name in before {
        assert!(after.contains(&name), "{name} was deleted by a failed run: {after:?}");
    }
}

/// The refusal does not need `-b`: `write_midx_included_packs()` passes
/// `--preferred-pack` whether or not a bitmap was asked for, and the check in
/// `write_midx_internal()` is on the preferred pack itself.
#[test]
fn refusal_does_not_depend_on_bitmaps() {
    let (repo, home) = fixture("nobitmap");

    let out = run(&repo, &home, &["repack", "-a", "--write-midx", "--filter=blob:none"]);

    assert_eq!(out.status.code(), Some(255), "exit status");
    let stem = empty_pack_stem(&repo);
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        format!("error: cannot select preferred pack .git/objects/pack/{stem}.pack with no objects\n"),
    );
    assert!(
        !pack_dir_entries(&repo).iter().any(|n| n.starts_with("multi-pack-index")),
        "left a midx behind"
    );
}

/// The second pack is only empty when the new pack covers the old ones. An
/// *incremental* run with nothing new to pack writes no main pack at all, so the
/// filtered pack inherits everything the old packs held — it is not empty, no
/// preferred pack is refused, and the MIDX is written. The notice still goes to
/// stdout, git printing it about the first `pack-objects` alone.
#[test]
fn incremental_filtered_pack_inherits_the_existing_packs() {
    let (repo, home) = fixture("incremental");

    let out = run(&repo, &home, &["repack", "-b", "--write-midx", "--filter=blob:none"]);

    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "Nothing new to pack.\n");
    let after = pack_dir_entries(&repo);
    assert!(after.contains(&"multi-pack-index".to_string()), "no midx written: {after:?}");
    assert!(
        pack_files(&repo).iter().all(|name| {
            let bytes = std::fs::read(repo.join(".git/objects/pack").join(name)).unwrap();
            u32::from_be_bytes(bytes[8..12].try_into().unwrap()) > 0
        }),
        "an empty pack was written: {after:?}"
    );
}

/// `finish_pack_objects_cmd()` keeps packs written outside the object store out
/// of `names` ("avoid putting packs written outside of the repository in the
/// list of names"), so sending the filtered pack elsewhere leaves only the main
/// pack to prefer and the MIDX is written. The same run without `--filter-to`
/// fails, which is what makes this an assertion about the locality rule rather
/// than about the filter.
///
/// Only the exit status and the repository's own `objects/pack` are asserted
/// here; where a `--filter-to` pack lands, and which values count as outside the
/// object store, is `repack_filter_to.rs`.
#[test]
fn filtered_pack_written_elsewhere_is_not_preferred() {
    let (repo, home) = fixture("filterto");
    let elsewhere = repo.parent().unwrap().join("elsewhere");
    let before = pack_files(&repo).len();

    let out = run(
        &repo,
        &home,
        &[
            "repack",
            "-a",
            "--write-midx",
            "--filter=blob:none",
            &format!("--filter-to={}", elsewhere.display()),
        ],
    );

    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let after = pack_dir_entries(&repo);
    assert!(after.contains(&"multi-pack-index".to_string()), "no midx written: {after:?}");
    assert_eq!(
        pack_files(&repo).len(),
        before + 1,
        "only the main pack belongs in the object store: {after:?}"
    );
}

/// `cmd_repack()` pushes `--write-bitmap-index` at `pack-objects` only
/// `if (write_midx == REPACK_WRITE_MIDX_NONE)`: under `--write-midx` the bitmap
/// belongs to the MIDX, and the packs are left bare. Without `--write-midx` the
/// same `-b` does write one, which is what pins this to the MIDX gate rather
/// than to bitmaps being off.
#[test]
fn write_midx_moves_the_bitmap_off_the_pack() {
    let (repo, home) = fixture("bitmap");
    let out = run(&repo, &home, &["repack", "-a", "-b", "--write-midx"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let after = pack_dir_entries(&repo);
    assert!(after.contains(&"multi-pack-index".to_string()), "no midx written: {after:?}");
    assert!(!after.iter().any(|n| n.ends_with(".bitmap")), "pack bitmap written: {after:?}");

    let (repo, home) = fixture("bitmap-nomidx");
    let out = run(&repo, &home, &["repack", "-a", "-b"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let after = pack_dir_entries(&repo);
    assert!(after.iter().any(|n| n.ends_with(".bitmap")), "no pack bitmap written: {after:?}");
}
