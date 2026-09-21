//! What `git fsck`'s object-directory scan covers, in what order it says so, and
//! the one tree check that is not a comparison of adjacent entries.
//!
//! Four claims, each a byte captured from stock git 2.55.0:
//!
//!   1. **`--no-full` is not just "skip `verify_pack`".** `fsck_source()` walks
//!      `.git/objects/??` and nothing else; the pack loop — whose
//!      `fsck_obj_buffer()` callback is the only thing that sets `HAS_OBJ` on a
//!      packed object — is what `check_full` gates (builtin/fsck.c:1072-1101).
//!      So under `--no-full` a packed object is never created, never linted,
//!      never traced, and every reflog entry naming one is an error, because
//!      `fsck_handle_reflog_oid()` tests `lookup_object()` + `HAS_OBJ` rather
//!      than reading the odb (:479-493). Nothing is reported `missing`, though:
//!      `check_reachable_object()` checks `has_object_pack()` first (:268-269).
//!   2. **`--verbose`'s `Checking <type> <oid>` line is part of the scan's stderr
//!      stream**, not a separate one: `fsck_obj()` prints it between
//!      `parse_object_buffer()` and `fsck_walk()` (:413-416), so a finding about
//!      an object follows that object's own line and precedes the next object's.
//!   3. **A tree's duplicate-entry check is not pairwise.** `verify_ordered()`
//!      keeps a stack of non-directory names that are prefixes of the next entry
//!      (fsck.c:579-611), so `x`, `x.1`, `x/` — three correctly ordered adjacent
//!      pairs — still reports `duplicateEntries`.
//!   4. **Reflogs are visited in sorted order.** `reflog_iterator_begin()` passes
//!      `DIR_ITERATOR_SORTED` (refs/files-backend.c:2409), which reads each
//!      directory whole and `string_list_sort()`s it (dir-iterator.c:116-132).
//!
//! Fixtures are built with the binary under test; nothing binary is checked in.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("TERM", "dumb")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run binary under test")
}

fn ok(dir: &Path, home: &Path, args: &[&str]) -> String {
    let out = run(dir, home, args);
    assert!(
        out.status.success(),
        "{args:?} failed ({}): {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn with_stdin(dir: &Path, home: &Path, args: &[&str], input: &str) -> String {
    use std::io::Write;
    let mut child = Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn binary under test");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{args:?} failed ({}): {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn fsck(dir: &Path, home: &Path, extra: &[&str]) -> (String, String, i32) {
    let mut args = vec!["fsck", "--no-progress"];
    args.extend_from_slice(extra);
    let out = run(dir, home, &args);
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-fsck-scope-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    let home = root.join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    ok(&repo, &home, &["init", "-q", "-b", "main", "."]);
    std::fs::write(repo.join("f.txt"), "hello\n").unwrap();
    ok(&repo, &home, &["add", "-A"]);
    ok(&repo, &home, &["commit", "-q", "-m", "one"]);
    std::fs::write(repo.join("f.txt"), "hello\ntwo\n").unwrap();
    ok(&repo, &home, &["commit", "-q", "-am", "two"]);
    (repo, home)
}

/// Claim 1.
#[test]
fn no_full_leaves_every_packed_object_uncreated() {
    let (repo, home) = fixture("nofull");
    ok(&repo, &home, &["repack", "-adq"]);
    assert!(
        std::fs::read_dir(repo.join(".git/objects"))
            .unwrap()
            .filter_map(Result::ok)
            .all(|e| {
                let n = e.file_name();
                let n = n.to_string_lossy();
                n == "pack" || n == "info"
            }),
        "the fixture only makes its point while nothing is left loose"
    );

    // `--full` (the default): everything is scanned and nothing is wrong.
    assert_eq!(fsck(&repo, &home, &[]), (String::new(), String::new(), 0));
    let (_, full_trace, _) = fsck(&repo, &home, &["-v"]);
    assert!(
        full_trace.contains("Checking commit "),
        "with --full the pack is scanned; got:\n{full_trace}"
    );

    let (stdout, stderr, code) = fsck(&repo, &home, &["--no-full"]);
    assert_eq!(
        stdout, "",
        "check_reachable_object() returns on has_object_pack() before it can \
         print `missing`; got:\n{stdout}"
    );
    for line in stderr.lines() {
        assert!(
            line.starts_with("error: ") && line.contains(": invalid reflog entry "),
            "the only --no-full finding is fsck_handle_reflog_oid()'s; got: {line}"
        );
    }
    assert!(!stderr.is_empty(), "this repository does have reflogs");
    assert_eq!(code, 2, "ERROR_REACHABLE");

    let (_, trace, _) = fsck(&repo, &home, &["--no-full", "-v"]);
    assert!(
        !trace.contains("Checking commit "),
        "fsck_source() walks .git/objects/?? only, and there is nothing there; got:\n{trace}"
    );
    assert!(trace.contains("Checking object directory\n"));
}

/// Claim 2: a finding sits between its own object's trace line and the next
/// object's.
#[test]
fn a_finding_follows_its_own_checking_line() {
    let (repo, home) = fixture("trace");
    // A commit whose author line has no space before the date. `--literally`
    // is what lets a malformed commit be written at all.
    let basis = ok(&repo, &home, &["cat-file", "commit", "HEAD"]);
    let bad = basis.replacen("@example.com>", "@example.com>>", 1);
    let new = with_stdin(
        &repo,
        &home,
        &["hash-object", "--literally", "-t", "commit", "-w", "--stdin"],
        &bad,
    )
    .trim()
    .to_owned();
    ok(&repo, &home, &["update-ref", "refs/heads/bogus", &new]);

    let (_, stderr, code) = fsck(&repo, &home, &["-v"]);
    let lines: Vec<&str> = stderr.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.starts_with("error in commit ") && l.contains(&new))
        .unwrap_or_else(|| panic!("no finding for {new} in:\n{stderr}"));
    assert_eq!(
        lines[at - 1],
        format!("Checking commit {new}"),
        "fsck_obj() prints its trace line before fsck_object() reports; got:\n{stderr}"
    );
    assert!(
        lines[at + 1].starts_with("Checking "),
        "and the next line belongs to the next object; got:\n{stderr}"
    );
    assert_eq!(code, 1);
}

/// Claim 3: the three `check_duplicate_names` sets of `t/t1450-fsck.sh`.
#[test]
fn a_directory_file_conflict_is_a_duplicate_even_when_not_adjacent() {
    for (tag, names) in [
        ("dn1", &["x", "x.1", "x/"][..]),
        ("dn2", &["x", "x.1.2", "x.1/", "x/"][..]),
        ("dn3", &["x", "x.1", "x.1.2", "x/"][..]),
    ] {
        let (repo, home) = fixture(tag);
        let blob = with_stdin(&repo, &home, &["hash-object", "-w", "--stdin"], "blob\n")
            .trim()
            .to_owned();
        let inner = with_stdin(&repo, &home, &["mktree"], &format!("100644 blob {blob}\tx.2\n"))
            .trim()
            .to_owned();
        let mut spec = String::new();
        for name in names {
            match name.strip_suffix('/') {
                Some(dir) => spec.push_str(&format!("040000 tree {inner}\t{dir}\n")),
                None => spec.push_str(&format!("100644 blob {blob}\t{name}\n")),
            }
        }
        let bad = with_stdin(&repo, &home, &["mktree"], &spec).trim().to_owned();

        let (_, stderr, code) = fsck(&repo, &home, &[]);
        assert_eq!(
            stderr,
            format!("error in tree {bad}: duplicateEntries: contains duplicate file entries\n"),
            "{names:?} names the same path twice once a directory's implicit \
             trailing slash is applied"
        );
        assert_eq!(code, 1, "duplicateEntries is error severity by default");
        // Every adjacent pair is in order, so a pairwise check would say nothing.
        assert!(!stderr.contains("treeNotSorted"));
    }
}

/// Claim 4.
///
/// Sixty logs, written in an order unrelated to their names and each naming an
/// object that does not exist, so that `fsck_handle_reflog_oid()` prints one
/// `<refname>: invalid reflog entry` line per file *in the order the files were
/// visited*. Sixty names make a coincidental match between `readdir()` order and
/// `strcmp` order about as likely as picking one permutation of sixty at random,
/// which is what makes this an actual test of the sort rather than of the
/// filesystem.
#[test]
fn reflogs_are_walked_in_sorted_order() {
    let (repo, home) = fixture("reflogsort");
    let hexsz = ok(&repo, &home, &["rev-parse", "HEAD"]).trim().len();
    let zero = "0".repeat(hexsz);
    let dir = repo.join(".git/logs/refs/heads");
    std::fs::create_dir_all(&dir).unwrap();
    // Write in a scrambled order (a stride of 23 over 60, which is coprime to
    // it, so every name is written exactly once and never in name order).
    let names: Vec<String> = (0..60).map(|i| format!("b{i:02}")).collect();
    for step in 0..60usize {
        let i = (step * 23) % 60;
        // A distinct absent object per file, so a line can be traced back to it.
        let absent = format!("{:0width$x}", 0xdead_0000u64 + i as u64, width = hexsz);
        std::fs::write(
            dir.join(&names[i]),
            format!("{zero} {absent} T <t@example.com> 1700000000 +0000\tbranch: created\n"),
        )
        .unwrap();
    }

    let (_, stderr, code) = fsck(&repo, &home, &[]);
    let seen: Vec<&str> = stderr
        .lines()
        .filter_map(|l| l.strip_prefix("error: refs/heads/"))
        .filter_map(|l| l.split(':').next())
        .filter(|n| n.starts_with('b'))
        .collect();
    let expected: Vec<&str> = names.iter().map(String::as_str).collect();
    assert_eq!(
        seen, expected,
        "DIR_ITERATOR_SORTED makes reflog_iterator_begin() hand \
         for_each_reflog() one directory's entries in strcmp order"
    );
    assert_eq!(code & 2, 2, "ERROR_REACHABLE");
}
