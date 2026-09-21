//! `git index-pack` splits its object checking across two independent flags.
//!
//! ```c
//! } else if (skip_to_optional_arg(arg, "--strict", &arg)) {
//!         strict = 1;
//!         do_fsck_object = 1;
//!         fsck_set_msg_types(&fsck_options, arg);
//! } else if (!strcmp(arg, "--check-self-contained-and-connected")) {
//!         strict = 1;
//!         check_self_contained_and_connected = 1;
//! } else if (skip_to_optional_arg(arg, "--fsck-objects", &arg)) {
//!         do_fsck_object = 1;
//!         fsck_set_msg_types(&fsck_options, arg);
//! }
//! ```
//!
//! (builtin/index-pack.c:1935-1944.) `do_fsck_object` runs `fsck_object()` over
//! every object and `fsck_finish()` at the end; `strict` runs
//! `parse_object_buffer()`'s gate and `fsck_walk()`/`check_objects()`'s link
//! pass. `--check-self-contained-and-connected` raises only the second, then:
//!
//! ```c
//! /*
//!  * Let the caller know this pack is not self contained
//!  */
//! if (check_self_contained_and_connected && foreign_nr)
//!         return 1;
//! ```
//!
//! (builtin/index-pack.c:2145-2149, with `foreign_nr = check_objects()` at
//! `:2090`.) `check_objects()` counts the linked objects it had to read out of
//! the object database rather than out of the pack, so a self-contained pack is
//! exit 0 and a thin one is exit 1 — after the `pack\t<hash>` line, not instead
//! of it. That status is how `fetch-pack` decides whether it still needs a
//! connectivity check, so answering 0 for a thin pack is a silent correctness
//! loss upstream; the port used to refuse the flag outright.
//!
//! The `strict` half also gates `parse_object_buffer()` (builtin/index-pack.c:950-954),
//! whose failure is `die(_("invalid %s"), type_name(type))` *before* any content
//! check — so a commit or tag the parser rejects never reaches `fsck_object()`
//! and never reports an fsck id.
//!
//! Every expectation here was captured from stock git 2.55.0.
#![cfg(unix)]

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

const NULL_OID: &str = "0000000000000000000000000000000000000000";

struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-ipself-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let fx = Fixture { root, work };
        fx.ok(&["init", "-q", "-b", "main", "."]);
        fx
    }

    fn feed(&self, args: &[&str], stdin: &[u8]) -> Output {
        let mut child = Command::new(BIN)
            .args([
                "-c",
                "user.email=t@e.co",
                "-c",
                "user.name=t",
                "-c",
                "gc.auto=0",
            ])
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("ZVCS_HOME", &self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.root.join("gitconfig"))
            .env("GIT_CONFIG_SYSTEM", self.root.join("gitconfig-system"))
            .env("GIT_AUTHOR_DATE", "@1000000000 +0000")
            .env("GIT_COMMITTER_DATE", "@1000000000 +0000")
            .env("LC_ALL", "C")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("run binary");
        child.stdin.take().unwrap().write_all(stdin).unwrap();
        child.wait_with_output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> Output {
        let out = self.feed(args, b"");
        assert!(out.status.success(), "setup git {args:?}: {out:?}");
        out
    }

    fn line(&self, args: &[&str]) -> String {
        String::from_utf8_lossy(&self.ok(args).stdout).trim_end().to_string()
    }

    /// One commit, and the ids a pack can be built from.
    fn commit(&self, name: &str, body: &str, message: &str) -> String {
        std::fs::write(self.work.join(name), body).unwrap();
        self.ok(&["add", name]);
        self.ok(&["commit", "-q", "-m", message]);
        self.line(&["rev-parse", "HEAD"])
    }

    /// A pack holding exactly the objects named on stdin, with no traversal.
    fn pack_of(&self, ids: &[&str]) -> Vec<u8> {
        let input = ids.iter().map(|id| format!("{id}\n")).collect::<String>();
        let out = self.feed(&["pack-objects", "--stdout"], input.as_bytes());
        assert!(out.status.success(), "pack-objects: {out:?}");
        assert!(out.stdout.starts_with(b"PACK"), "pack-objects wrote no pack: {out:?}");
        out.stdout
    }

    /// A loose object of `kind` with exactly `body`, written past every check.
    fn literal(&self, kind: &str, body: &str) -> String {
        let out = self.feed(
            &["hash-object", "-t", kind, "--literally", "-w", "--stdin"],
            body.as_bytes(),
        );
        assert!(out.status.success(), "hash-object -t {kind}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim_end().to_string()
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Index `pack` into a throwaway bare repository that already holds everything
/// `donor` holds, so a thin pack's bases resolve out of the object database.
fn index_into_clone(donor: &Fixture, tag: &str, pack: &[u8], args: &[&str]) -> Output {
    let dst = donor.root.join(format!("dst-{tag}"));
    let _ = std::fs::remove_dir_all(&dst);
    donor.ok(&["clone", "-q", "--bare", ".", dst.to_str().unwrap()]);

    let mut full = vec!["index-pack", "--stdin"];
    full.extend_from_slice(args);
    let mut child = Command::new(BIN)
        .args(&full)
        .current_dir(&dst)
        .env("HOME", &donor.root)
        .env("ZVCS_HOME", &donor.root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", donor.root.join("gitconfig"))
        .env("GIT_CONFIG_SYSTEM", donor.root.join("gitconfig-system"))
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run binary");
    child.stdin.take().unwrap().write_all(pack).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn a_self_contained_pack_is_accepted_and_a_thin_one_is_exit_one() {
    let fx = Fixture::new("status");
    let head = fx.commit("f.txt", "hello\n", "one");
    let tree = fx.line(&["rev-parse", "HEAD^{tree}"]);
    let blob = fx.line(&["rev-parse", "HEAD:f.txt"]);

    // Commit + tree + blob: every link the commit makes is inside the pack, so
    // `check_objects()` reads nothing out of the object database.
    let whole = fx.pack_of(&[&head, &tree, &blob]);
    let out = index_into_clone(&fx, "whole", &whole, &["--check-self-contained-and-connected"]);
    assert_eq!(out.status.code(), Some(0), "self-contained pack: {out:?}");
    assert_eq!(stderr(&out), "", "{out:?}");
    assert!(stdout(&out).starts_with("pack\t"), "{out:?}");

    // The commit alone. Its tree is only reachable through the object database
    // the clone already has, which is exactly what `foreign_nr` counts.
    let thin = fx.pack_of(&[&head]);
    let out = index_into_clone(&fx, "thin", &thin, &["--check-self-contained-and-connected"]);
    assert_eq!(out.status.code(), Some(1), "thin pack: {out:?}");
    assert_eq!(stderr(&out), "", "the status is the only report: {out:?}");
    assert!(
        stdout(&out).starts_with("pack\t"),
        "the pack line is still printed before the status: {out:?}"
    );

    // Without the flag the very same thin pack is a plain success, because
    // nothing consults `foreign_nr`.
    let out = index_into_clone(&fx, "thin-plain", &thin, &[]);
    assert_eq!(out.status.code(), Some(0), "thin pack, no flag: {out:?}");
    // And `--strict` alone raises `strict` without raising the reporting flag.
    let out = index_into_clone(&fx, "thin-strict", &thin, &["--strict"]);
    assert_eq!(out.status.code(), Some(0), "thin pack, --strict: {out:?}");
}

#[test]
fn the_flag_runs_the_link_pass_without_running_the_content_checks() {
    let fx = Fixture::new("split");
    fx.commit("f.txt", "hello\n", "one");

    // A commit whose `tree` line is well formed — so `parse_commit_buffer()`
    // accepts it — but which has no `author`, which `fsck_commit()` rejects.
    // `--fsck-objects` and `--strict` must report the fsck id; the
    // self-contained flag must not, because `do_fsck_object` stays off. It
    // instead dies in the link pass, on the tree the commit names and nothing
    // has.
    let id = fx.literal("commit", &format!("tree {NULL_OID}\n\nmsg\n"));
    let pack = fx.pack_of(&[&id]);

    for flag in ["--fsck-objects", "--strict"] {
        let out = index_into_clone(&fx, &flag[2..], &pack, &[flag]);
        assert_eq!(out.status.code(), Some(128), "{flag}: {out:?}");
        assert_eq!(
            stderr(&out),
            format!(
                "error: object {id}: missingAuthor: invalid format - expected 'author' line\n\
                 fatal: fsck error in packed object\n"
            ),
            "{flag}"
        );
    }

    let out = index_into_clone(&fx, "self", &pack, &["--check-self-contained-and-connected"]);
    assert_eq!(out.status.code(), Some(128), "{out:?}");
    assert_eq!(
        stderr(&out),
        format!("fatal: did not receive expected object {NULL_OID}\n"),
        "the content check must not have run: {out:?}"
    );
}

#[test]
fn the_parser_gate_beats_the_content_checks() {
    let fx = Fixture::new("gate");
    fx.commit("f.txt", "hello\n", "one");

    // `parse_object_buffer()` runs first and its failure is a `die()`, so
    // neither of these ever reaches an fsck id. `parse_tag_buffer()` fails
    // silently; `parse_commit_buffer()` prints `bogus commit object <oid>` on
    // its way out.
    let tag = fx.literal("tag", &format!("object {NULL_OID}\n"));
    let commit = fx.literal("commit", "notree\n\nmsg\n");

    for flag in ["--strict", "--fsck-objects", "--check-self-contained-and-connected"] {
        let out = index_into_clone(&fx, &format!("tag{}", &flag[2..4]), &fx.pack_of(&[&tag]), &[flag]);
        assert_eq!(out.status.code(), Some(128), "tag {flag}: {out:?}");
        assert_eq!(stderr(&out), "fatal: invalid tag\n", "tag {flag}");

        let out = index_into_clone(
            &fx,
            &format!("commit{}", &flag[2..4]),
            &fx.pack_of(&[&commit]),
            &[flag],
        );
        assert_eq!(out.status.code(), Some(128), "commit {flag}: {out:?}");
        assert_eq!(
            stderr(&out),
            format!("error: bogus commit object {commit}\nfatal: invalid commit\n"),
            "commit {flag}"
        );
    }

    // Without either flag nothing parses anything: both packs index cleanly.
    for (label, id) in [("tag", &tag), ("commit", &commit)] {
        let out = index_into_clone(&fx, &format!("{label}-plain"), &fx.pack_of(&[id]), &[]);
        assert_eq!(out.status.code(), Some(0), "{label} without a flag: {out:?}");
    }
}

#[test]
fn the_first_failing_object_is_the_last_one_reported() {
    let fx = Fixture::new("first");
    fx.commit("f.txt", "hello\n", "one");

    // `die()` fires inside `sha1_object()`, so a pack holding two broken objects
    // reports one line, not two. Which one depends on pack order, so the
    // assertion is on the count and the shape rather than on a particular id.
    let a = fx.literal("commit", &format!("tree {NULL_OID}\n\na\n"));
    let b = fx.literal("commit", &format!("tree {NULL_OID}\n\nbb\n"));
    let out = index_into_clone(&fx, "two", &fx.pack_of(&[&a, &b]), &["--fsck-objects"]);

    assert_eq!(out.status.code(), Some(128), "{out:?}");
    let lines: Vec<String> = stderr(&out).lines().map(str::to_string).collect();
    assert_eq!(lines.len(), 2, "one report plus one fatal expected: {out:?}");
    assert!(
        lines[0].ends_with("missingAuthor: invalid format - expected 'author' line"),
        "{out:?}"
    );
    assert!(
        lines[0] == format!("error: object {a}: missingAuthor: invalid format - expected 'author' line")
            || lines[0] == format!("error: object {b}: missingAuthor: invalid format - expected 'author' line"),
        "the reported id must be one of the two broken commits: {out:?}"
    );
    assert_eq!(lines[1], "fatal: fsck error in packed object", "{out:?}");
}
