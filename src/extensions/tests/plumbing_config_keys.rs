//! Configuration keys the object-database plumbing reads, pinned to the exact
//! behaviour git 2.55.0 was observed to produce for each value.
//!
//! Every expectation below was taken from a differential run against stock git
//! (`git -c <key>=<value> <cmd>` under a byte-identical environment) and is
//! asserted here as a literal so the parity survives without stock git being
//! installed — these run headless with nothing on `PATH` but the binary under
//! test.
//!
//! Covered:
//!   * `core.maxTreeDepth` — `read-tree`'s recursion fail-safe.
//!   * `core.fsync` / `core.fsyncMethod` / `core.fsyncObjectFiles` — the
//!     hardening policy and its three diagnostics.
//!   * `index.skipHash` and `index.recordEndOfIndexEntries` (with its
//!     `index.threads` default) — the index write options.
//!   * `pack.packSizeLimit` — `pack-objects` / `repack`'s 1 MiB floor warning and
//!     its validation ahead of parse-options.
//!   * `gc.recentObjectsHook` — extra "recent" traversal tips for `prune`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Run the binary under test in `repo` with an isolated environment, so no
/// ambient global or system config can reach the run.
fn run(repo: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("ZVCS_HOME", home)
        .env("LC_ALL", "C")
        .output()
        .unwrap()
}

/// Same, asserting success — used to build fixtures, never as behaviour under test.
fn ok(repo: &Path, home: &Path, args: &[&str]) -> Output {
    let out = run(repo, home, args);
    assert!(
        out.status.success(),
        "setup `git {args:?}` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A repository whose only blob sits four directories deep, plus an isolated
/// empty `HOME`. The depth is what makes `core.maxTreeDepth`'s boundary
/// observable: `a/b/c/d/f.txt` is reached from a tree at recursion depth 4.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-plcfg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(repo.join("a/b/c/d")).unwrap();
    std::fs::create_dir_all(repo.join("sub")).unwrap();
    let root = root.canonicalize().unwrap();
    let (home, repo) = (root.join("home"), root.join("repo"));
    std::fs::write(repo.join("a/b/c/d/f.txt"), "hi\n").unwrap();
    std::fs::write(repo.join("sub/s"), "s\n").unwrap();
    ok(&repo, &home, &["init", "-q", "-b", "main"]);
    ok(&repo, &home, &["config", "user.email", "alice@example.com"]);
    ok(&repo, &home, &["config", "user.name", "Alice"]);
    ok(&repo, &home, &["add", "-A"]);
    ok(&repo, &home, &["commit", "-q", "-m", "c0"]);
    (repo, home)
}

fn head_tree(repo: &Path, home: &Path) -> String {
    stdout(&ok(repo, home, &["rev-parse", "HEAD^{tree}"])).trim().to_string()
}

// ---------------------------------------------------------------------------
// core.maxTreeDepth
// ---------------------------------------------------------------------------

/// The boundary is exact, and it is the *number of directory levels*: the tree
/// holding `a/b/c/d/f.txt` is reached at depth 4, so a limit of 3 rejects it and
/// a limit of 4 accepts it. Off by one in either direction and this test fails.
#[test]
fn max_tree_depth_boundary_is_the_directory_level_count() {
    let (repo, home) = fixture("depth");
    let tree = head_tree(&repo, &home);

    let rejected = run(&repo, &home, &["-c", "core.maxTreeDepth=3", "read-tree", &tree]);
    assert_eq!(rejected.status.code(), Some(128));
    assert_eq!(stderr(&rejected), "error: exceeded maximum allowed tree depth\n");

    let accepted = run(&repo, &home, &["-c", "core.maxTreeDepth=4", "read-tree", &tree]);
    assert!(accepted.status.success(), "stderr: {}", stderr(&accepted));
    assert_eq!(stderr(&accepted), "");
}

/// The value goes through `git_config_int`, not `git_config_ulong`: a negative
/// value is a number that rejects every tree, while an unreadable one is fatal
/// with git's `die_bad_number` wording — including the ` in file <path>` origin
/// clause when it came from a config file rather than `-c`.
#[test]
fn max_tree_depth_is_a_signed_int_with_gits_bad_number_diagnostic() {
    let (repo, home) = fixture("depthint");
    let tree = head_tree(&repo, &home);

    let negative = run(&repo, &home, &["-c", "core.maxTreeDepth=-1", "read-tree", &tree]);
    assert_eq!(negative.status.code(), Some(128));
    assert_eq!(stderr(&negative), "error: exceeded maximum allowed tree depth\n");

    let from_cli = run(&repo, &home, &["-c", "core.maxTreeDepth=abc", "read-tree", &tree]);
    assert_eq!(from_cli.status.code(), Some(128));
    assert_eq!(
        stderr(&from_cli),
        "fatal: bad numeric config value 'abc' for 'core.maxtreedepth': invalid unit\n"
    );

    ok(&repo, &home, &["config", "core.maxTreeDepth", "abc"]);
    let from_file = run(&repo, &home, &["read-tree", &tree]);
    assert_eq!(from_file.status.code(), Some(128));
    assert_eq!(
        stderr(&from_file),
        "fatal: bad numeric config value 'abc' for 'core.maxtreedepth' in file .git/config: \
         invalid unit\n"
    );
}

/// The rejection happens before the index is touched, and a `--prefix` costs no
/// levels — git starts the prefix as `read_tree_at()`'s base string at depth 0,
/// so a one-level tree binds under a three-segment prefix even at depth 0.
#[test]
fn max_tree_depth_leaves_the_index_alone_and_ignores_the_prefix() {
    let (repo, home) = fixture("depthidx");
    let tree = head_tree(&repo, &home);
    let deep = stdout(&ok(&repo, &home, &["rev-parse", "HEAD:a/b/c/d"])).trim().to_string();

    ok(&repo, &home, &["read-tree", "--empty"]);
    ok(&repo, &home, &["read-tree", "--prefix=z/", &deep]);
    let before = stdout(&ok(&repo, &home, &["ls-files"]));
    assert_eq!(before, "z/f.txt\n");

    let rejected = run(&repo, &home, &["-c", "core.maxTreeDepth=1", "read-tree", &tree]);
    assert_eq!(rejected.status.code(), Some(128));
    assert_eq!(stdout(&ok(&repo, &home, &["ls-files"])), before, "index was modified");

    ok(&repo, &home, &["read-tree", "--empty"]);
    let bound = run(&repo, &home, &["-c", "core.maxTreeDepth=0", "read-tree", "--prefix=p/q/r/", &deep]);
    assert!(bound.status.success(), "stderr: {}", stderr(&bound));
    assert_eq!(stdout(&ok(&repo, &home, &["ls-files"])), "p/q/r/f.txt\n");
}

// ---------------------------------------------------------------------------
// core.fsync / core.fsyncMethod / core.fsyncObjectFiles
// ---------------------------------------------------------------------------

/// Each unreadable piece of the hardening config produces git's own warning and
/// the run continues; a valid component list and method are silent.
#[test]
fn fsync_config_diagnostics_match_git() {
    let (repo, home) = fixture("fsync");
    let write = |args: &[&str]| {
        let mut full = args.to_vec();
        full.extend_from_slice(&["update-index", "--force-write-index"]);
        run(&repo, &home, &full)
    };

    let unknown_component = write(&["-c", "core.fsync=bogus"]);
    assert!(unknown_component.status.success());
    assert_eq!(
        stderr(&unknown_component),
        "warning: ignoring unknown core.fsync component 'bogus'\n"
    );

    // An unknown name inside an otherwise good list warns once and the rest of
    // the list still applies.
    let mixed = write(&["-c", "core.fsync=all,-loose-object,bogus"]);
    assert!(mixed.status.success());
    assert_eq!(stderr(&mixed), "warning: ignoring unknown core.fsync component 'bogus'\n");

    assert_eq!(stderr(&write(&["-c", "core.fsync=index,pack"])), "");
    assert_eq!(stderr(&write(&["-c", "core.fsyncMethod=batch"])), "");
    assert_eq!(
        stderr(&write(&["-c", "core.fsyncMethod=bogus"])),
        "warning: ignoring unknown core.fsyncMethod value 'bogus'\n"
    );
    assert_eq!(stderr(&write(&[])), "", "an unset policy must be silent");
}

/// `core.fsyncObjectFiles` warns whenever it is set at all, and *then* dies if
/// the value is not a boolean — the deprecation notice comes first, on the same
/// stderr, which is the ordering git produces.
#[test]
fn fsync_object_files_warns_before_it_dies() {
    let (repo, home) = fixture("fsyncdep");

    let deprecated = run(
        &repo,
        &home,
        &["-c", "core.fsyncObjectFiles=true", "update-index", "--force-write-index"],
    );
    assert!(deprecated.status.success());
    assert_eq!(
        stderr(&deprecated),
        "warning: core.fsyncObjectFiles is deprecated; use core.fsync instead\n"
    );

    let bad = run(
        &repo,
        &home,
        &["-c", "core.fsyncObjectFiles=bogus", "update-index", "--force-write-index"],
    );
    assert_eq!(bad.status.code(), Some(128));
    assert_eq!(
        stderr(&bad),
        "warning: core.fsyncObjectFiles is deprecated; use core.fsync instead\n\
         fatal: bad boolean config value 'bogus' for 'core.fsyncobjectfiles'\n"
    );
}

// ---------------------------------------------------------------------------
// index.recordEndOfIndexEntries
// ---------------------------------------------------------------------------

/// The `EOIE` extension signature, as it appears in a written index.
const EOIE: &[u8] = b"EOIE";

/// The trailing checksum `index.skipHash` zeroes, which is the object hash's
/// width at the very end of the file.
fn index_trailer(repo: &Path) -> Vec<u8> {
    let bytes = std::fs::read(repo.join(".git/index")).unwrap();
    bytes[bytes.len() - 20..].to_vec()
}

/// `index.skipHash=true` writes an all-zero trailing checksum instead of
/// computing one, and an unset key computes it — the observable half of the
/// index-write options this port derives from configuration.
#[test]
fn skip_hash_zeroes_the_index_trailer() {
    let (repo, home) = fixture("skiphash");

    let skipped = run(&repo, &home, &["-c", "index.skipHash=true", "update-index", "--force-write-index"]);
    assert!(skipped.status.success(), "stderr: {}", stderr(&skipped));
    assert_eq!(index_trailer(&repo), vec![0u8; 20], "skipHash must zero the trailer");

    let computed = run(&repo, &home, &["update-index", "--force-write-index"]);
    assert!(computed.status.success(), "stderr: {}", stderr(&computed));
    assert_ne!(index_trailer(&repo), vec![0u8; 20], "an unset skipHash must compute the trailer");
}

/// `EOIE` records where an index's extensions begin, so git only appends it when
/// some *other* extension was written. This port writes none — it cannot
/// recompute a tree-cache and never marks an index sparse — so the extension can
/// never appear here regardless of `index.recordEndOfIndexEntries`.
///
/// That is exactly why this assertion exists: the key's default is "false unless
/// `index.threads` is enabled", and a writer that emitted `EOIE` unconditionally
/// (which is `gix_index::write::Extensions::All`, the default this port used
/// before the key was read) would produce an index git 2.55.0 does not. The
/// stock-git-seeded half of that comparison lives in the differential harness;
/// what is pinned here is that no combination of the two keys makes this port
/// emit an extension it has no other extension to index.
#[test]
fn end_of_index_entry_is_never_written_without_another_extension() {
    let (repo, home) = fixture("eoie");

    for prefix in [
        vec![],
        vec!["-c", "index.recordEndOfIndexEntries=true"],
        vec!["-c", "index.recordEndOfIndexEntries=false"],
        vec!["-c", "index.threads=4"],
        vec!["-c", "index.threads=1"],
    ] {
        let mut args = prefix.clone();
        args.extend_from_slice(&["update-index", "--force-write-index"]);
        let out = run(&repo, &home, &args);
        assert!(out.status.success(), "{prefix:?}: {}", stderr(&out));
        let bytes = std::fs::read(repo.join(".git/index")).unwrap();
        assert!(
            !bytes.windows(EOIE.len()).any(|w| w == EOIE),
            "{prefix:?} wrote an EOIE with no extension to index"
        );
    }
}

// ---------------------------------------------------------------------------
// pack.packSizeLimit
// ---------------------------------------------------------------------------

/// git's 1 MiB floor warning, verbatim.
const MIN_PACK_SIZE_WARNING: &str = "warning: minimum pack size limit is 1 MiB\n";

/// The config supplies `--max-pack-size`'s default, warns below git's floor, is
/// never adopted when writing to stdout, and loses to an explicit option.
#[test]
fn pack_size_limit_warns_below_one_mib_except_on_stdout() {
    let (repo, home) = fixture("packlimit");

    let below = run(&repo, &home, &["-c", "pack.packSizeLimit=1024", "pack-objects", "--all", "p1"]);
    assert!(below.status.success());
    assert_eq!(stderr(&below), MIN_PACK_SIZE_WARNING);

    // `pack_size_limit_cfg` is only adopted when *not* packing to stdout, so a
    // sub-floor config is silent there rather than warning or dying.
    let to_stdout =
        run(&repo, &home, &["-c", "pack.packSizeLimit=1024", "pack-objects", "--all", "--stdout"]);
    assert!(to_stdout.status.success());
    assert_eq!(stderr(&to_stdout), "");

    // Zero is git's "unlimited", and a value at or above the floor is fine.
    for value in ["0", "2m"] {
        let arg = format!("pack.packSizeLimit={value}");
        let out = run(&repo, &home, &["-c", &arg, "pack-objects", "--all", "p2"]);
        assert!(out.status.success());
        assert_eq!(stderr(&out), "", "pack.packSizeLimit={value} must be silent");
    }

    // An explicit `--max-pack-size` shadows the config entirely.
    let overridden = run(
        &repo,
        &home,
        &["-c", "pack.packSizeLimit=1024", "pack-objects", "--all", "--max-pack-size=2m", "p3"],
    );
    assert!(overridden.status.success());
    assert_eq!(stderr(&overridden), "");

    // repack forwards the same limit to its packing step, so it warns too.
    let repacked = run(&repo, &home, &["-c", "pack.packSizeLimit=1024", "repack", "-ad"]);
    assert!(repacked.status.success());
    assert_eq!(stderr(&repacked), MIN_PACK_SIZE_WARNING);
}

/// git reads the pack config before `parse_options`, so an unreadable value is
/// fatal ahead of every parse diagnostic — including `-h`, which would otherwise
/// print usage and exit 129.
#[test]
fn pack_size_limit_is_validated_before_parse_options() {
    let (repo, home) = fixture("packlimitbad");
    const MESSAGE: &str =
        "fatal: bad numeric config value 'bogus' for 'pack.packsizelimit': invalid unit\n";

    for args in [
        vec!["-c", "pack.packSizeLimit=bogus", "pack-objects", "-h"],
        vec!["-c", "pack.packSizeLimit=bogus", "pack-objects", "--nosuchoption"],
        vec!["-c", "pack.packSizeLimit=bogus", "repack", "-h"],
    ] {
        let out = run(&repo, &home, &args);
        assert_eq!(out.status.code(), Some(128), "{args:?}");
        assert_eq!(stdout(&out), "", "{args:?} must print no usage block");
        assert_eq!(stderr(&out), MESSAGE, "{args:?}");
    }
}

// ---------------------------------------------------------------------------
// gc.recentObjectsHook
// ---------------------------------------------------------------------------

/// git's failure line when any configured hook does not exit 0.
const HOOK_FAILED: &str = "fatal: unable to enumerate additional recent objects\n";

/// Write an executable `/bin/sh` script into `repo` and return its relative path.
fn script(repo: &Path, name: &str, body: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    let path = repo.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    format!("./{name}")
}

/// An id the hook names is kept "regardless of its true age", so the unreachable
/// blob `prune --expire=now` would otherwise remove survives; an id the
/// repository does not hold is ignored rather than being an error.
#[test]
fn recent_objects_hook_keeps_the_ids_it_names() {
    let (repo, home) = fixture("hookkeep");
    let blob = {
        let out = Command::new(BIN)
            .args(["hash-object", "-w", "--stdin"])
            .current_dir(&repo)
            .env("HOME", &home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("ZVCS_HOME", &home)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write as _;
                child.stdin.take().unwrap().write_all(b"unreachable\n")?;
                child.wait_with_output()
            })
            .unwrap();
        stdout(&out).trim().to_string()
    };

    // Without a hook the blob is unreachable and reported for pruning.
    let bare = run(&repo, &home, &["prune", "--expire=now", "-n"]);
    assert!(bare.status.success(), "stderr: {}", stderr(&bare));
    assert!(stdout(&bare).contains(&blob), "expected {blob} to be prunable: {}", stdout(&bare));

    let names_it = script(&repo, "keep.sh", &format!("echo {blob}\n"));
    let kept = run(&repo, &home, &["-c", &format!("gc.recentObjectsHook={names_it}"), "prune", "--expire=now", "-n"]);
    assert!(kept.status.success(), "stderr: {}", stderr(&kept));
    assert_eq!(stdout(&kept), "", "the hook's id must survive the prune");

    // An id the repository does not hold is silently ignored.
    let unknown = script(&repo, "unknown.sh", "echo 0000000000000000000000000000000000000000\n");
    let ignored = run(&repo, &home, &["-c", &format!("gc.recentObjectsHook={unknown}"), "prune", "--expire=now", "-n"]);
    assert!(ignored.status.success(), "stderr: {}", stderr(&ignored));
    assert!(stdout(&ignored).contains(&blob), "an unknown id must not keep anything");
}

/// Every failure mode halts the prune with git's wording and exit 128: a hook
/// that exits non-zero, a hook that prints something that is not an object id,
/// and a hook naming a program that cannot be exec'd.
#[test]
fn recent_objects_hook_failures_halt_the_prune() {
    let (repo, home) = fixture("hookfail");
    let failing = script(&repo, "fail.sh", "exit 3\n");
    let garbage = script(&repo, "garbage.sh", "echo notanoid\n");

    let exited_nonzero = run(&repo, &home, &["-c", &format!("gc.recentObjectsHook={failing}"), "prune", "--expire=now", "-n"]);
    assert_eq!(exited_nonzero.status.code(), Some(128));
    assert_eq!(stderr(&exited_nonzero), HOOK_FAILED);

    let bad_line = run(&repo, &home, &["-c", &format!("gc.recentObjectsHook={garbage}"), "prune", "--expire=now", "-n"]);
    assert_eq!(bad_line.status.code(), Some(128));
    assert_eq!(
        stderr(&bad_line),
        format!("error: invalid extra cruft tip: 'notanoid'\n{HOOK_FAILED}")
    );

    // A command with no shell metacharacters is exec'd directly, which is why the
    // diagnostic is git's own `cannot exec` rather than a shell's.
    let missing = run(&repo, &home, &["-c", "gc.recentObjectsHook=/nonexistent-zvcs-hook", "prune", "--expire=now", "-n"]);
    assert_eq!(missing.status.code(), Some(128));
    assert_eq!(
        stderr(&missing),
        format!(
            "fatal: cannot exec '/nonexistent-zvcs-hook': No such file or directory\n{HOOK_FAILED}"
        )
    );
}

/// The hook set rides on the same `mark_recent` gate as the loose/packed mtime
/// scans, so `--expire=never` (git's `0`) never runs it — not even to fail.
#[test]
fn recent_objects_hook_is_skipped_when_nothing_is_recent() {
    let (repo, home) = fixture("hooknever");
    let out = run(&repo, &home, &["-c", "gc.recentObjectsHook=/nonexistent-zvcs-hook", "prune", "--expire=never", "-n"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert_eq!(stderr(&out), "");
}
