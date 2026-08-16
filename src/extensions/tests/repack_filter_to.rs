//! `repack --filter-to=<value>`: the value is a pack *prefix*, and the locality
//! rule that decides whether the pack it names is installed.
//!
//! `write_filtered_pack()` (`repack-filtered.c:18`) hands `opts->destination`
//! straight to `pack-objects` as its `base-name` positional, so the artifacts are
//! `<value>-<hash>.pack` / `.idx` / `.rev`. Nothing about `<value>` is treated as
//! a directory: git creates none, and a `<value>` that happens to name an
//! existing directory produces a *sibling* of it.
//!
//! What then happens to that pack is `write_pack_opts_is_local()`
//! (`repack.c:86-89`), which is `starts_with(opts->destination, opts->packdir)`
//! over the two strings with neither resolved. Three outcomes, and the tests
//! below are one apiece:
//!
//! ```text
//!   * non-local  → kept out of `names`, so it is never installed as
//!                  `pack-<hash>` and never eligible as `--preferred-pack`; it
//!                  simply stays where it was written, and the run exits 0.
//!   * local      → put in `names`, but `generated_pack_populate()` looked for
//!                  its artifacts under `packtmp` and found none, so
//!                  `generated_pack_install()` dies 128 — after the pack was
//!                  written at the prefix and before anything was installed.
//!   * absent     → the destination is `packtmp` itself, which is local, is where
//!                  the artifacts are, and so is installed beside the main pack.
//! ```
//!
//! The middle case is the one that makes this a test of the *string* rule rather
//! than of paths: `--filter-to=.git/objects/pack/zz` fails while
//! `--filter-to=$PWD/.git/objects/pack/zz` succeeds, naming the same directory.
//!
//! Every expectation below was read off git 2.55.0 on the same fixture first. No
//! hash is spelled out: each is discovered from the names on disk, so the
//! assertions hold for any hash function and do not depend on this port and git
//! packing to identical bytes.

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

/// A repository in two packs of three objects each, plus an empty `out`
/// directory beside it for the `--filter-to` destinations.
///
/// Two packs are what make an *incremental* `repack -d --filter` interesting:
/// nothing is left to pack, so no new pack subtracts from the filtered one and
/// it inherits all six objects. That is a filtered pack with contents to assert
/// about rather than the empty one an `-a` run produces here.
fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-rpfilterto-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    let out = root.join("out");
    for dir in [&home, &repo, &out] {
        std::fs::create_dir_all(dir).unwrap();
    }
    git(&repo, &home, &["init", "-q", "-b", "main"]);
    git(&repo, &home, &["config", "user.email", "alice@example.com"]);
    git(&repo, &home, &["config", "user.name", "Alice"]);
    for name in ["f", "g"] {
        std::fs::write(repo.join(name), format!("contents of {name}\n")).unwrap();
        git(&repo, &home, &["add", name]);
        git(&repo, &home, &["commit", "-q", "-m", name]);
        git(&repo, &home, &["repack", "-d", "-q"]);
    }
    assert_eq!(names_in(&repo.join(".git/objects/pack"), ".pack").len(), 2, "fixture needs 2 packs");
    (root, repo, home)
}

/// The names in `dir` ending in `suffix`, sorted.
fn names_in(dir: &Path, suffix: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(suffix))
        .collect();
    names.sort();
    names
}

/// The object count in the v2 pack header of `path`.
fn object_count(path: &Path) -> u32 {
    let bytes = std::fs::read(path).unwrap();
    u32::from_be_bytes(bytes[8..12].try_into().unwrap())
}

/// The one name in `dir` that starts with `stem` and ends with `suffix`.
fn only_named(dir: &Path, stem: &str, suffix: &str) -> String {
    let found: Vec<String> =
        names_in(dir, suffix).into_iter().filter(|n| n.starts_with(stem)).collect();
    assert_eq!(found.len(), 1, "expected one {stem}*{suffix} in {dir:?}, got {found:?}");
    found.into_iter().next().unwrap()
}

/// `--filter-to=<dir>/pfx` writes `<dir>/pfx-<hash>.{pack,idx,rev}` — not
/// `<dir>/pfx/pack-<hash>.*`, and not anything named `pack-<hash>` at all.
#[test]
fn filter_to_is_a_pack_prefix() {
    let (root, repo, home) = fixture("prefix");
    let out = root.join("out");

    let status = run(
        &repo,
        &home,
        &[
            "repack",
            "-d",
            "-q",
            "--filter=blob:none",
            &format!("--filter-to={}", out.join("pfx").display()),
        ],
    );
    assert_eq!(status.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&status.stderr));

    let written = names_in(&out, "");
    assert_eq!(written.len(), 3, "expected exactly the three artifacts: {written:?}");
    let pack = only_named(&out, "pfx-", ".pack");
    for ext in [".idx", ".rev"] {
        let companion = pack.replace(".pack", ext);
        assert!(written.contains(&companion), "{companion} missing: {written:?}");
    }
    // A directory of that name would have been the other reading of the option.
    assert!(!out.join("pfx").is_dir(), "--filter-to created a directory");

    // The incremental run packed nothing new, so the filtered pack inherits every
    // object the two existing packs held.
    assert_eq!(object_count(&out.join(&pack)), 6, "filtered pack contents");
    // Non-local, so it is not one of the repository's own packs.
    assert_eq!(
        names_in(&repo.join(".git/objects/pack"), ".pack").len(),
        2,
        "the object store gained a pack"
    );
}

/// A `<value>` that names an existing directory is still just a prefix, so the
/// pack lands *beside* that directory and the directory itself stays empty.
#[test]
fn filter_to_naming_a_directory_writes_beside_it() {
    let (root, repo, home) = fixture("isdir");
    let out = root.join("out");

    let status = run(
        &repo,
        &home,
        &["repack", "-d", "-q", "--filter=blob:none", &format!("--filter-to={}", out.display())],
    );
    assert_eq!(status.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&status.stderr));

    assert_eq!(names_in(&out, "").len(), 0, "the directory itself was written into");
    let pack = only_named(&root, "out-", ".pack");
    assert_eq!(object_count(&root.join(&pack)), 6, "filtered pack contents");
}

/// git creates no directory for `--filter-to`: `pack-objects` writes its
/// temporary files under the object store and then cannot move them into a
/// directory that is not there. Both lines, exit 128, nothing installed.
#[test]
fn filter_to_creates_no_directory() {
    let (root, repo, home) = fixture("nodir");
    let missing = root.join("nope");
    let before = names_in(&repo.join(".git/objects/pack"), ".pack");

    let status = run(
        &repo,
        &home,
        &[
            "repack",
            "-d",
            "-q",
            "--filter=blob:none",
            &format!("--filter-to={}", missing.join("pfx").display()),
        ],
    );

    assert_eq!(status.status.code(), Some(128), "exit status");
    assert!(!missing.exists(), "--filter-to created its parent directory");
    let stderr = String::from_utf8_lossy(&status.stderr).into_owned();
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines.len(), 2, "expected two lines, got {stderr:?}");
    let prefix = missing.join("pfx");
    let named = lines[0]
        .strip_prefix(&format!("error: unable to write file {}-", prefix.display()))
        .and_then(|rest| rest.strip_suffix(".pack: No such file or directory"))
        .unwrap_or_else(|| panic!("unexpected first line: {:?}", lines[0]));
    assert!(named.chars().all(|c| c.is_ascii_hexdigit()), "not a pack hash: {named}");
    assert_eq!(
        lines[1],
        format!("fatal: unable to rename temporary file to '{}-{named}.pack'", prefix.display()),
    );

    // The failure is before `generated_pack_install()`, so the object store is
    // exactly as it was.
    assert_eq!(names_in(&repo.join(".git/objects/pack"), ".pack"), before, "packs changed");
}

/// A `--filter-to` that *is* local — it starts with the `packdir` string git
/// built — puts the pack in `names` without putting its artifacts where
/// `generated_pack_populate()` looks for them, so `generated_pack_install()`
/// dies. The pack is written at the prefix all the same, and nothing else moves.
#[test]
fn a_local_filter_to_cannot_be_installed() {
    let (_root, repo, home) = fixture("local");
    let pack_dir = repo.join(".git/objects/pack");
    let before = names_in(&pack_dir, ".pack");

    let status = run(
        &repo,
        &home,
        &["repack", "-d", "-q", "--filter=blob:none", "--filter-to=.git/objects/pack/zz"],
    );

    assert_eq!(status.status.code(), Some(128), "exit status");
    // Written, and named after its own hash, exactly as a non-local one is.
    let pack = only_named(&pack_dir, "zz-", ".pack");
    let hash = pack.trim_start_matches("zz-").trim_end_matches(".pack");
    assert_eq!(object_count(&pack_dir.join(&pack)), 6, "filtered pack contents");

    let stderr = String::from_utf8_lossy(&status.stderr).into_owned();
    let head = "fatal: pack-objects did not write a '.pack' file for pack .git/objects/pack/.tmp-";
    let pid_and_rest = stderr
        .strip_prefix(head)
        .unwrap_or_else(|| panic!("unexpected stderr: {stderr:?}"));
    let (pid, rest) = pid_and_rest.split_once("-pack-").expect("packtmp shape");
    assert!(pid.chars().all(|c| c.is_ascii_digit()), "not a pid: {pid}");
    assert_eq!(rest, format!("{hash}\n"), "the message names the pack that was written");

    // `generated_pack_install()` dies on the first pack in `names`, so neither
    // the main pack nor `-d`'s deletions happened; the only new file in the
    // directory is the filtered pack the prefix put there.
    let installed: Vec<String> =
        names_in(&pack_dir, ".pack").into_iter().filter(|n| n.starts_with("pack-")).collect();
    assert_eq!(installed, before, "the object store changed");
}

/// The same directory named absolutely is *not* local, because git compares
/// strings and its `packdir` is the relative `.git/objects/pack`. So the run
/// succeeds, the pack stays under the name it was given, and it is not installed
/// as one of the repository's own.
#[test]
fn an_absolute_filter_to_into_the_object_store_is_not_local() {
    let (_root, repo, home) = fixture("abs");
    let pack_dir = repo.join(".git/objects/pack");

    let status = run(
        &repo,
        &home,
        &[
            "repack",
            "-a",
            "-d",
            "-q",
            "--filter=blob:none",
            &format!("--filter-to={}", pack_dir.join("zz").display()),
        ],
    );
    assert_eq!(status.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&status.stderr));

    // `-a -d` leaves exactly the one pack it wrote; the filtered pack is not one
    // of them, so it was neither installed nor considered for deletion.
    let installed: Vec<String> =
        names_in(&pack_dir, ".pack").into_iter().filter(|n| n.starts_with("pack-")).collect();
    assert_eq!(installed.len(), 1, "expected one installed pack: {installed:?}");
    let filtered = only_named(&pack_dir, "zz-", ".pack");
    // Everything is in the main pack, the index naming both blobs, so the
    // filtered pack has nothing left to hold.
    assert_eq!(object_count(&pack_dir.join(&filtered)), 0, "filtered pack contents");
    assert_eq!(object_count(&pack_dir.join(&installed[0])), 6, "main pack contents");
}
