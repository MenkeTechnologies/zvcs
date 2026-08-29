//! `git archive --add-file` / `--add-virtual-file`, whose in-archive names come
//! from two different prefixes.
//!
//! `add_file_cb()` (archive.c:562-624) reads `--prefix` through `opt->defval`
//! while `parse_options()` is still walking the command line, so each record
//! keeps the prefix that was in effect *when it was parsed* — a later `--prefix`
//! never reaches it. The cwd prefix goes the other way: `--add-file` puts it on
//! the path it reads from disk, `--add-virtual-file` puts it on the name in the
//! archive.
//!
//! The names are read back out of the tar stream directly rather than by shelling
//! out to `tar`, so the assertions are about the bytes git writes.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

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

/// The `name` field of every `ustar` header in `tar`, in order. A 512-byte
/// header whose name field is empty ends the stream.
fn names(tar: &[u8]) -> Vec<String> {
    let mut found = Vec::new();
    let mut at = 0;
    while at + 512 <= tar.len() {
        let header = &tar[at..at + 512];
        let end = header[..100].iter().position(|b| *b == 0).unwrap_or(100);
        if end == 0 {
            break;
        }
        let name = String::from_utf8_lossy(&header[..end]).into_owned();
        // The size field is 11 octal digits followed by a space or NUL.
        let size = std::str::from_utf8(&header[124..135])
            .ok()
            .and_then(|s| u64::from_str_radix(s.trim_matches(|c| c == ' ' || c == '\0'), 8).ok())
            .unwrap_or(0);
        at += 512 + (size as usize).div_ceil(512) * 512;
        // The pax header git writes for the commit id is not an archived path.
        if name != "pax_global_header" {
            found.push(name);
        }
    }
    found
}

/// A repository with one file at the root and one in a subdirectory.
fn fixture(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-addfile-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).expect("mkdir fixture");
    std::fs::write(root.join("README.md"), "hi\n").expect("write README.md");
    std::fs::write(root.join("src").join("lib.rs"), "fn main(){}\n").expect("write lib.rs");
    ok(&root, &["init", "-q", "-b", "main"]);
    ok(&root, &["add", "README.md", "src/lib.rs"]);
    ok(&root, &["commit", "-qm", "x"]);
    root
}

#[test]
fn an_added_file_keeps_the_prefix_that_was_in_effect_when_it_was_parsed() {
    let dir = fixture("order");

    // `--prefix` after the record does not reach it: the file lands at the root.
    let after = ok(&dir, &["archive", "--format=tar", "--add-file=src/lib.rs", "--prefix=p/", "HEAD"]);
    assert_eq!(names(&after.stdout), ["p/", "p/README.md", "p/src/", "p/src/lib.rs", "lib.rs"]);

    // `--prefix` before it does.
    let before = ok(&dir, &["archive", "--format=tar", "--prefix=p/", "--add-file=src/lib.rs", "HEAD"]);
    assert_eq!(names(&before.stdout), ["p/", "p/README.md", "p/src/", "p/src/lib.rs", "p/lib.rs"]);

    // Each record keeps its own, so two records can carry two different prefixes
    // even though only the last `--prefix` reaches the tree itself.
    let both = ok(
        &dir,
        &[
            "archive",
            "--format=tar",
            "--prefix=a/",
            "--add-file=src/lib.rs",
            "--prefix=b/",
            "--add-file=README.md",
            "HEAD",
        ],
    );
    assert_eq!(
        names(&both.stdout),
        ["b/", "b/README.md", "b/src/", "b/src/lib.rs", "a/lib.rs", "b/README.md"]
    );

    // `--no-prefix` in between takes it away again for what follows.
    let cleared = ok(
        &dir,
        &[
            "archive",
            "--format=tar",
            "--prefix=a/",
            "--add-file=src/lib.rs",
            "--no-prefix",
            "--add-file=README.md",
            "HEAD",
        ],
    );
    assert_eq!(names(&cleared.stdout), ["README.md", "src/", "src/lib.rs", "a/lib.rs", "README.md"]);
}

#[test]
fn a_virtual_files_name_is_c_unquoted_and_takes_the_cwd_prefix_not_the_archive_prefix() {
    let dir = fixture("virtual");

    // A quoted name is decoded, and the colon must be the byte right after it.
    let quoted = ok(&dir, &["archive", "--format=tar", "--add-virtual-file=\"quoted.txt\":body", "HEAD"]);
    assert_eq!(names(&quoted.stdout), ["README.md", "src/", "src/lib.rs", "quoted.txt"]);

    // An empty decode is indistinguishable from no decode, so `""` stays literal.
    let empty = ok(&dir, &["archive", "--format=tar", "--add-virtual-file=\"\":x", "HEAD"]);
    assert_eq!(empty.stdout.is_empty(), false);
    assert_eq!(names(&empty.stdout).last().map(String::as_str), Some("\"\""));

    let unclosed = run(&dir, &["archive", "--format=tar", "--add-virtual-file=\"abc:x", "HEAD"]);
    assert_eq!(unclosed.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&unclosed.stderr),
        "fatal: unclosed quote: '\"abc:x'\n"
    );

    // `--prefix` never reaches a virtual file…
    let prefixed = ok(&dir, &["archive", "--format=tar", "--prefix=p/", "--add-virtual-file=v.txt:body", "HEAD"]);
    assert_eq!(names(&prefixed.stdout).last().map(String::as_str), Some("v.txt"));

    // …but the cwd prefix does, which is the reverse of `--add-file`, where the
    // cwd prefix reaches the path on disk and `--prefix` the name in the archive.
    let sub = dir.join("src");
    let from_sub = ok(&sub, &["archive", "--format=tar", "--prefix=p/", "--add-virtual-file=v.txt:body", "--add-file=lib.rs", "HEAD"]);
    assert_eq!(names(&from_sub.stdout), ["p/", "p/lib.rs", "src/v.txt", "p/lib.rs"]);
}

/// `--remote` hands the whole command line to `git upload-archive` on the far
/// side and copies back what it sends: `ACK`, a flush, then the archive on
/// sideband 1. A server that failed reports on band 2 and band 3, and the client
/// exits 1 without writing an archive.
#[test]
fn a_remote_archive_is_fetched_over_the_upload_archive_protocol() {
    let dir = fixture("remote");

    let local = ok(&dir, &["archive", "--format=tar", "HEAD"]);
    let remote = ok(&dir, &["archive", "--remote=.", "--format=tar", "HEAD"]);
    assert_eq!(remote.stdout, local.stdout, "the same archive, over the protocol");

    // Options travel verbatim, so `--prefix` is applied by the far side.
    let prefixed = ok(&dir, &["archive", "--remote=.", "--format=tar", "--prefix=r/", "HEAD"]);
    assert_eq!(names(&prefixed.stdout), ["r/", "r/README.md", "r/src/", "r/src/lib.rs"]);

    // `-o` names the file the archive lands in, and its extension is sent ahead
    // of the rest as a `--format` the far side may still be told to override.
    let out = dir.join("out.tgz");
    let written = ok(&dir, &["archive", "--remote=.", "-o", out.to_str().expect("utf-8"), "HEAD"]);
    assert!(written.stdout.is_empty(), "the archive went to the file, not to stdout");
    let bytes = std::fs::read(&out).expect("read out.tgz");
    assert_eq!(&bytes[..3], b"\x1f\x8b\x08", "gzip magic: the .tgz suffix picked the format");

    // A repository the server cannot open: band 2 carries its diagnostic as a
    // `remote:` line, band 3 the archiver's death, and nothing reaches stdout.
    let missing = run(&dir, &["archive", "--remote=nosuchdir", "HEAD"]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(missing.stdout.is_empty(), "no archive on a failed fetch");
    let err = String::from_utf8_lossy(&missing.stderr);
    assert!(
        err.contains("remote: fatal: 'nosuchdir' does not appear to be a git repository"),
        "{err}"
    );
    assert!(err.contains("remote: git upload-archive: archiver died with error"), "{err}");
}
