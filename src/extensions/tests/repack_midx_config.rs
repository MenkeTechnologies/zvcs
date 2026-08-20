//! The three `repack.midx*` keys, which `repack_config()` reads before
//! parse-options (`builtin/repack.c:97-110`) and whose range checks run after it
//! (`:291-296`).
//!
//! Two of them steer the geometric merge of *incremental* MIDX layers, a mode
//! this port refuses outright, so what is asserted for those is the part that is
//! independent of it: git runs both range checks unconditionally, with no
//! `--write-midx` on the line and nothing to pack, and names the command-line
//! option the key shadows rather than the key itself.
//!
//! `repack.midxMustContainCruft` is different — it decides which packs the
//! `multi-pack-index` covers, and that is asserted as behavior: a repository
//! holding a cruft pack gets a MIDX with or without it, and the two overrides git
//! applies to the key are exercised too.
//!
//! Every literal below was captured from git 2.55.0 (`/opt/homebrew/bin/git`);
//! the commands are named in each test.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LC_ALL", "C")
        .output()
        .expect("run binary")
}

fn ok(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    let out = run(cwd, home, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

/// A one-commit repository plus an isolated, empty `HOME`.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-midxcfg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    ok(&repo, &home, &["init", "-q", "-b", "main"]);
    ok(&repo, &home, &["config", "user.email", "alice@example.com"]);
    ok(&repo, &home, &["config", "user.name", "Alice"]);
    commit(&repo, &home, "f", "hello\n", "c0");
    (repo, home)
}

fn commit(repo: &Path, home: &Path, path: &str, body: &str, message: &str) {
    std::fs::write(repo.join(path), body).unwrap();
    ok(repo, home, &["add", path]);
    ok(repo, home, &["commit", "-q", "-m", message]);
}

#[test]
fn midx_split_factor_and_new_layer_threshold_reject_out_of_range_values() {
    // git 2.55.0, in a repository with nothing to pack:
    //     $ git -c repack.midxSplitFactor=1 repack
    //     fatal: invalid value for --midx-split-factor: 1
    //     $ git -c repack.midxNewLayerThreshold=0 repack
    //     fatal: invalid value for --midx-new-layer-threshold: 0
    // Neither run asks for a MIDX at all: `builtin/repack.c:291-296` is
    // unconditional. The diagnostic names the option, not the key, and prints the
    // value as the `int` `git_config_int` read.
    let (repo, home) = fixture("range");

    for value in ["1", "0", "-1", "-2147483648"] {
        let out = run(&repo, &home, &["-c", &format!("repack.midxSplitFactor={value}"), "repack", "-q"]);
        assert_eq!(
            stderr(&out),
            format!("fatal: invalid value for --midx-split-factor: {value}\n"),
            "repack.midxSplitFactor={value} is below git's floor of 2"
        );
        assert_eq!(code(&out), 128);
    }
    for value in ["0", "-1"] {
        let out = run(
            &repo,
            &home,
            &["-c", &format!("repack.midxNewLayerThreshold={value}"), "repack", "-q"],
        );
        assert_eq!(
            stderr(&out),
            format!("fatal: invalid value for --midx-new-layer-threshold: {value}\n"),
            "repack.midxNewLayerThreshold={value} is below git's floor of 1"
        );
        assert_eq!(code(&out), 128);
    }

    // In range, both are accepted and the repack proceeds.
    for args in [
        ["repack.midxSplitFactor=2", "repack.midxNewLayerThreshold=1"],
        ["repack.midxSplitFactor=64", "repack.midxNewLayerThreshold=4096"],
    ] {
        let out = run(&repo, &home, &["-c", args[0], "-c", args[1], "repack", "-q"]);
        assert!(out.status.success(), "{args:?} must be accepted: {}", stderr(&out));
        assert!(stderr(&out).is_empty(), "{args:?} must be silent");
    }

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn midx_keys_are_parsed_before_option_parsing_and_range_checked_after() {
    // The two halves land on opposite sides of `parse_options`, and `-h` is what
    // separates them. git 2.55.0:
    //     $ git -c repack.midxSplitFactor=abc repack -h
    //     fatal: bad numeric config value 'abc' for 'repack.midxsplitfactor': invalid unit
    //     $ git -c repack.midxSplitFactor=1 repack -h
    //     usage: git repack …                                        (exit 129)
    let (repo, home) = fixture("order");

    for (key, lowered) in [
        ("repack.midxSplitFactor", "repack.midxsplitfactor"),
        ("repack.midxNewLayerThreshold", "repack.midxnewlayerthreshold"),
    ] {
        let out = run(&repo, &home, &["-c", &format!("{key}=abc"), "repack", "-h"]);
        assert_eq!(
            stderr(&out),
            format!("fatal: bad numeric config value 'abc' for '{lowered}': invalid unit\n"),
            "{key} is read by repack_config, ahead of parse-options"
        );
        assert_eq!(code(&out), 128);
        assert!(out.stdout.is_empty());
    }

    let out = run(&repo, &home, &["-c", "repack.midxMustContainCruft=bogus", "repack", "-h"]);
    assert_eq!(
        stderr(&out),
        "fatal: bad boolean config value 'bogus' for 'repack.midxmustcontaincruft'\n"
    );
    assert_eq!(code(&out), 128);

    // The range check is the other side: `-h` wins over it.
    let help = run(&repo, &home, &["-c", "repack.midxSplitFactor=1", "repack", "-h"]);
    assert_eq!(code(&help), 129, "an out-of-range value must not beat -h");
    assert!(help.stdout.starts_with(b"usage: git repack"));
    assert!(stderr(&help).is_empty());

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn midx_range_check_sits_between_the_bitmap_and_filter_to_refusals() {
    // git's order inside `cmd_repack()`: the incremental-bitmap conflict (:274),
    // then the two range checks (:291-296), then `--filter-to` without `--filter`
    // (:407). Pinned against git 2.55.0 by running each pair — `-c
    // repack.midxSplitFactor=1 repack -b` reports the bitmap conflict, while the
    // same config with `--filter-to=x` reports the split factor.
    let (repo, home) = fixture("ordering-pairs");

    let bitmaps = run(&repo, &home, &["-c", "repack.midxSplitFactor=1", "repack", "-b", "-q"]);
    assert_eq!(
        stderr(&bitmaps),
        "fatal: Incremental repacks are incompatible with bitmap indexes.  Use\n\
         --no-write-bitmap-index or disable the pack.writeBitmaps configuration.\n",
        "the bitmap conflict is checked first"
    );

    let filter_to = run(
        &repo,
        &home,
        &["-c", "repack.midxSplitFactor=1", "repack", "--filter-to=x", "-q"],
    );
    assert_eq!(
        stderr(&filter_to),
        "fatal: invalid value for --midx-split-factor: 1\n",
        "the range check beats the --filter-to pairing"
    );

    // And between the two keys, the split factor is checked first.
    let both = run(
        &repo,
        &home,
        &[
            "-c",
            "repack.midxSplitFactor=1",
            "-c",
            "repack.midxNewLayerThreshold=0",
            "repack",
            "-q",
        ],
    );
    assert_eq!(stderr(&both), "fatal: invalid value for --midx-split-factor: 1\n");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// The `.idx` names a `multi-pack-index` lists, read straight out of the file.
///
/// The chunk holding them is a run of NUL-terminated names, so the file is
/// scanned for the `pack-<hex>.idx` shape rather than parsed — enough to answer
/// "is this pack covered", which is the only question here.
fn midx_pack_names(repo: &Path) -> Vec<String> {
    let bytes = std::fs::read(repo.join(".git/objects/pack/multi-pack-index"))
        .expect("multi-pack-index must exist");
    let mut names = Vec::new();
    let mut current = String::new();
    for &b in &bytes {
        if b.is_ascii_graphic() {
            current.push(b as char);
        } else {
            if current.contains("pack-") && current.ends_with(".idx") {
                let at = current.find("pack-").unwrap();
                names.push(current[at..].to_string());
            }
            current.clear();
        }
    }
    names.sort();
    names
}

/// The single cruft pack's `.idx` name, from its `.mtimes` sidecar.
fn cruft_idx_name(repo: &Path) -> String {
    let dir = repo.join(".git/objects/pack");
    let mut found: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|n| n.strip_suffix(".mtimes").map(|base| format!("{base}.idx")))
        .collect();
    found.sort();
    assert_eq!(found.len(), 1, "fixture must hold exactly one cruft pack, found {found:?}");
    found.pop().unwrap()
}

/// A repository with one reachable pack and one cruft pack, which is the only
/// shape in which `repack.midxMustContainCruft` has anything to decide.
fn cruft_fixture(tag: &str) -> (PathBuf, PathBuf) {
    let (repo, home) = fixture(tag);
    commit(&repo, &home, "junk", "unreachable\n", "junk");
    ok(&repo, &home, &["reset", "-q", "--hard", "HEAD~1"]);
    ok(&repo, &home, &["reflog", "expire", "--expire=now", "--all"]);
    ok(&repo, &home, &["gc", "-q"]);
    assert!(
        repo.join(".git/objects/pack").read_dir().unwrap().flatten().any(|e| e
            .file_name()
            .to_string_lossy()
            .ends_with(".mtimes")),
        "fixture must have produced a cruft pack"
    );
    (repo, home)
}

#[test]
fn midx_must_contain_cruft_decides_whether_the_index_covers_the_cruft_pack() {
    // `repack-midx.c:199-235`: with the key false and nothing forcing it back on,
    // the cruft packs are left out of the `multi-pack-index`. git 2.55.0 on the
    // same fixture (one reachable pack, one cruft pack, one new commit so the run
    // writes a pack of its own) lists three packs under `true` and two under
    // `false`, the missing one being the cruft pack.
    // The two halves are separate repositories: a commit embeds the wall clock,
    // so neither the commit ids nor the pack names they hash to repeat between
    // fixture builds. Each half therefore reads its own cruft pack's name.
    let (repo, home) = cruft_fixture("cruft-true");
    let cruft_name = cruft_idx_name(&repo);
    commit(&repo, &home, "b", "b\n", "c1");
    ok(&repo, &home, &["-c", "repack.midxMustContainCruft=true", "repack", "--write-midx", "-q"]);
    let names = midx_pack_names(&repo);
    assert!(
        names.contains(&cruft_name),
        "true must keep the cruft pack in the MIDX; got {names:?}"
    );
    assert!(names.len() > 1, "and the non-cruft packs too; got {names:?}");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());

    let (repo, home) = cruft_fixture("cruft-false");
    let cruft_name = cruft_idx_name(&repo);
    commit(&repo, &home, "b", "b\n", "c1");
    ok(&repo, &home, &["-c", "repack.midxMustContainCruft=false", "repack", "--write-midx", "-q"]);
    let names = midx_pack_names(&repo);
    assert!(
        !names.contains(&cruft_name),
        "false must drop the cruft pack from the MIDX; got {names:?}"
    );
    assert!(!names.is_empty(), "the non-cruft packs are still covered");
    assert!(
        repo.join(".git/objects/pack").join(cruft_name.replace(".idx", ".pack")).exists(),
        "the cruft pack itself is untouched; only the MIDX's coverage changed"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn midx_must_contain_cruft_is_forced_on_when_no_pack_was_written_and_no_midx_exists() {
    // `builtin/repack.c:460-478`: with nothing new to pack, the surviving
    // non-cruft packs may reference objects only the cruft pack still holds, so
    // the MIDX has to cover them — unless a MIDX already exists to answer the
    // question instead. git 2.55.0 keeps the cruft pack in this case even with
    // the key set to false, which is what makes this a guard rather than a
    // duplicate of the test above.
    let (repo, home) = cruft_fixture("cruft-forced");
    let cruft_name = cruft_idx_name(&repo);
    assert!(
        !repo.join(".git/objects/pack/multi-pack-index").exists(),
        "the fixture must start without a MIDX"
    );

    ok(&repo, &home, &["-c", "repack.midxMustContainCruft=false", "repack", "--write-midx", "-q"]);
    let names = midx_pack_names(&repo);
    assert!(
        names.contains(&cruft_name),
        "a run that packed nothing into a repository with no MIDX must still cover the cruft pack; got {names:?}"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
