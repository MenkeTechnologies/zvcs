//! `rev-list --test-bitmap`: the reader's side of the `.bitmap` format.
//!
//! The option decompresses one commit's stored bitmap, reports its width, its
//! `ewah_checksum()` and the pack or multi-pack index that held it, then walks
//! the history for real and compares the two object sets. Every one of those
//! lines depends on reading the file correctly, and a reader that is wrong
//! about the XOR chain, the two coordinate systems or the type bitmaps prints
//! a plausible line that says something else — so the checks here are on the
//! exact text, not on the exit code alone.
//!
//! The repository is written by the binary under test, so a round trip failing
//! means the writer and the reader disagree about the format one of them is
//! implementing.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// The binary under test with the ambient user and system config kept out, so
/// a global `pack.*` cannot change what gets written.
fn cmd(repo: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(BIN);
    c.args(args)
        .current_dir(repo)
        .env("HOME", repo.join(".isolated-home"))
        .env("GIT_CONFIG_NOSYSTEM", "1");
    c
}

fn git(repo: &Path, args: &[&str]) {
    let out = cmd(repo, args).output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn test_bitmap(repo: &Path, args: &[&str]) -> (Output, String) {
    let mut all = vec!["rev-list", "--test-bitmap"];
    all.extend_from_slice(args);
    let out = cmd(repo, &all).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (out, stderr)
}

/// A repository with a handful of commits over several paths, so the pack has
/// commits, trees and blobs to tell apart, plus a tag that is never bitmapped.
fn fixture(tag: &str) -> PathBuf {
    let repo = std::env::temp_dir().join(format!("zvcs-testbitmap-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&repo);
    std::fs::create_dir_all(&repo).unwrap();
    // Canonical, so macOS's symlinked temporary directory does not make the
    // repository path differ from the one the binary reports.
    let repo = repo.canonicalize().unwrap();
    std::fs::create_dir_all(repo.join(".isolated-home")).unwrap();

    git(&repo, &["init", "-q", "-b", "main", "."]);
    git(&repo, &["config", "user.email", "alice@example.com"]);
    git(&repo, &["config", "user.name", "Alice"]);
    for n in 0..5 {
        std::fs::create_dir_all(repo.join(format!("d{n}"))).unwrap();
        std::fs::write(repo.join(format!("d{n}/f")), format!("content {n}\n")).unwrap();
        std::fs::write(repo.join("top"), format!("top {n}\n")).unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", &format!("c{n}")]);
    }
    git(&repo, &["tag", "-a", "v1", "-m", "tag one", "HEAD~2"]);
    repo
}

fn head(repo: &Path) -> String {
    let out = cmd(repo, &["rev-parse", "HEAD"]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// The one `<prefix>*.bitmap` in the object store, whose name carries the
/// checksum the `Located via` line has to report.
fn only_bitmap(repo: &Path, prefix: &str) -> PathBuf {
    let mut found: Vec<PathBuf> = std::fs::read_dir(repo.join(".git/objects/pack"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
            name.starts_with(prefix) && name.ends_with(".bitmap")
        })
        .collect();
    found.sort();
    assert_eq!(found.len(), 1, "expected exactly one {prefix}*.bitmap: {found:?}");
    found.pop().unwrap()
}

#[test]
fn a_pack_bitmap_this_binary_wrote_verifies_against_a_real_walk() {
    let repo = fixture("pack");
    git(&repo, &["-c", "pack.threads=1", "repack", "-adq", "--write-bitmap-index"]);

    let (out, stderr) = test_bitmap(&repo, &[&head(&repo)]);
    assert!(out.status.success(), "verification failed: {stderr}");
    assert!(out.stdout.is_empty(), "every line of this goes to stderr");

    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(
        lines.first(),
        Some(&"Bitmap v1 test (5 entries loaded, 5 total)"),
        "one entry per commit, all read up front because no lookup table was written"
    );
    assert!(
        lines[1].starts_with(&format!("Found bitmap for '{}'. ", head(&repo)))
            && lines[1].ends_with(" checksum"),
        "the commit, its bit width and its checksum: {}",
        lines[1]
    );
    let pack = only_bitmap(&repo, "pack-");
    let name = pack.file_stem().unwrap().to_string_lossy();
    let hash = name.strip_prefix("pack-").unwrap();
    assert_eq!(lines[2], format!("Located via pack '{hash}'."));
    assert_eq!(lines.last(), Some(&"OK!"), "the walk agreed with the bitmap");
}

#[test]
fn the_bit_width_and_checksum_belong_to_the_commit_that_was_asked_for() {
    let repo = fixture("distinct");
    git(&repo, &["-c", "pack.threads=1", "repack", "-adq", "--write-bitmap-index"]);

    let reported = |rev: &str| -> String {
        let (out, stderr) = test_bitmap(&repo, &[rev]);
        assert!(out.status.success(), "{rev} did not verify: {stderr}");
        stderr
            .lines()
            .find(|line| line.starts_with("Found bitmap for"))
            .expect("a bitmapped commit reports its bitmap")
            .to_owned()
    };
    assert_ne!(
        reported("HEAD"),
        reported("HEAD~1"),
        "two commits reach different object sets, so neither the width nor the \
         checksum may be shared — a reader that returns the same bitmap for \
         every commit would pass every other check here"
    );
}

#[test]
fn a_multi_pack_bitmap_is_preferred_and_names_the_index_it_belongs_to() {
    let repo = fixture("midx");
    git(&repo, &["-c", "pack.threads=1", "repack", "-adq", "--write-bitmap-index"]);
    git(&repo, &["multi-pack-index", "write", "--bitmap"]);

    let (out, stderr) = test_bitmap(&repo, &[&head(&repo)]);
    assert!(out.status.success(), "verification failed: {stderr}");
    let midx = only_bitmap(&repo, "multi-pack-index-");
    let name = midx.file_stem().unwrap().to_string_lossy();
    let hash = name.strip_prefix("multi-pack-index-").unwrap();
    assert!(
        stderr.contains(&format!("Located via MIDX '{hash}'.")),
        "a multi-pack bitmap wins over the pack bitmap beside it: {stderr}"
    );
    assert!(stderr.ends_with("OK!\n"), "{stderr}");
}

#[test]
fn a_commit_with_no_entry_is_reported_after_the_file_is_read() {
    let repo = fixture("noentry");
    git(&repo, &["-c", "pack.threads=1", "repack", "-adq", "--write-bitmap-index"]);
    let tag = {
        let out = cmd(&repo, &["rev-parse", "v1"]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };

    let (out, stderr) = test_bitmap(&repo, &[&tag]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        stderr,
        format!(
            "Bitmap v1 test (5 entries loaded, 5 total)\n\
             fatal: commit '{tag}' doesn't have an indexed bitmap\n"
        ),
        "the header is printed first, because git reports the file it loaded \
         before it looks the commit up"
    );
}

#[test]
fn the_operand_count_is_checked_after_the_bitmap_is_loaded() {
    let repo = fixture("operands");
    git(&repo, &["-c", "pack.threads=1", "repack", "-adq", "--write-bitmap-index"]);
    for args in [vec![], vec!["HEAD", "HEAD~1"], vec!["HEAD~1..HEAD"]] {
        let (out, stderr) = test_bitmap(&repo, &args);
        assert_eq!(out.status.code(), Some(128), "{args:?}");
        assert_eq!(stderr, "fatal: you must specify exactly one commit to test\n", "{args:?}");
    }
}

#[test]
fn a_repository_with_no_bitmap_says_so_before_anything_else() {
    let repo = fixture("none");
    git(&repo, &["-c", "pack.threads=1", "repack", "-adq"]);
    // Even with no operand at all, which is the later of the two checks.
    for args in [vec![], vec!["HEAD"]] {
        let (out, stderr) = test_bitmap(&repo, &args);
        assert_eq!(out.status.code(), Some(128), "{args:?}");
        assert_eq!(stderr, "fatal: failed to load bitmap indexes\n", "{args:?}");
    }
}

#[test]
fn a_corrupt_bitmap_is_reported_by_what_is_wrong_with_it() {
    let repo = fixture("corrupt");
    git(&repo, &["-c", "pack.threads=1", "repack", "-adq", "--write-bitmap-index"]);
    let path = only_bitmap(&repo, "pack-");
    let mut bytes = std::fs::read(&path).unwrap();

    // git installs pack artifacts read-only, so each rewrite replaces the file.
    let rewrite = |bytes: &[u8]| {
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, bytes).unwrap();
    };

    bytes[0] = b'X';
    rewrite(&bytes);
    let (out, stderr) = test_bitmap(&repo, &["HEAD"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        stderr,
        "error: corrupted bitmap index file (wrong header)\n\
         fatal: failed to load bitmap indexes\n"
    );

    bytes[0] = b'B';
    bytes[4..6].copy_from_slice(&9u16.to_be_bytes());
    rewrite(&bytes);
    let (out, stderr) = test_bitmap(&repo, &["HEAD"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        stderr,
        "error: unsupported version '9' for bitmap index file\n\
         fatal: failed to load bitmap indexes\n"
    );

    bytes[4..6].copy_from_slice(&1u16.to_be_bytes());
    rewrite(&bytes[..30]);
    let (out, stderr) = test_bitmap(&repo, &["HEAD"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        stderr,
        "error: corrupted bitmap index (too small)\n\
         fatal: failed to load bitmap indexes\n"
    );
}

#[test]
fn a_bitmap_that_disagrees_with_the_history_is_a_mismatch_and_not_an_ok() {
    let repo = fixture("mismatch");
    git(&repo, &["-c", "pack.threads=1", "repack", "-adq", "--write-bitmap-index"]);
    let path = only_bitmap(&repo, "pack-");
    let bytes = std::fs::read(&path).unwrap();

    // Walk past the header and the four type bitmaps to the first entry, then
    // past its own six-byte header into the compressed words, and flip a bit
    // there. The file still decodes; it just claims a different object set.
    let ewah_len = |at: usize| {
        let words = u32::from_be_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        8 + words * 8 + 4
    };
    let mut at = 12 + 20;
    for _ in 0..4 {
        at += ewah_len(at);
    }
    let words_at = at + 6 + 8;

    let mut broken = bytes.clone();
    broken[words_at + 8] ^= 0x01;
    let _ = std::fs::remove_file(&path);
    std::fs::write(&path, &broken).unwrap();

    let (out, stderr) = test_bitmap(&repo, &["HEAD"]);
    assert_eq!(out.status.code(), Some(128), "{stderr}");
    assert!(
        stderr.ends_with("fatal: mismatch in bitmap results\n"),
        "the walk must be real enough to notice: {stderr}"
    );
    assert!(
        stderr.contains("Found bitmap for"),
        "and the report must come after the bitmap was found: {stderr}"
    );
}
