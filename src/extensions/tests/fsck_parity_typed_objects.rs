//! `obj_hash` binds a type to an object id the first time anything looks it up, and
//! `git fsck` reports every later lookup of that id as another type.
//!
//! `lookup_commit()` / `lookup_tree()` / `lookup_blob()` / `lookup_tag()` go through
//! `object_as_type()`, which prints `error: object <oid> is a <bound>, not a <asked>` and
//! yields NULL for an id already typed otherwise (object.c:165-183). Each caller turns the
//! NULL into its own failure: the scan's `object corrupt or missing`, `fsck_obj()`'s `broken
//! links`, `mark_object()`'s `broken link from <type> unknown` pair, `parse_commit_buffer()`'s
//! `bad parent`, `parse_tag_buffer()`'s `bad tag pointer`, `snapshot_ref()`'s `invalid sha1
//! pointer`, `fsck_index()` dropping the entry, and a walk of the bound type's `Object <oid>
//! not a <type>`. zvcs read each object's real type from the odb instead and reported none of
//! it. Which lookup comes first is git's own order — snapshot, scan, refs, index, walk — so
//! the same corruption reads differently under `--connectivity-only`, which skips the scan.
//!
//! Each fixture is built once with stock git and copied, so both binaries see the same
//! bytes. Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    code: i32,
    stdout: String,
    stderr: String,
}

fn git(bin: &str, dir: &Path, args: &[&str], stdin: &[u8]) -> Outcome {
    let mut child = Command::new(bin)
        .args(["-c", "maintenance.auto=false", "-c", "gc.auto=0"])
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("GIT_EDITOR", "false")
        .env("TERM", "dumb")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    let out = child.wait_with_output().unwrap();
    Outcome {
        code: out.status.code().expect("no signal"),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        match entry.file_type().unwrap().is_dir() {
            true => copy_dir(&entry.path(), &target),
            false => {
                std::fs::copy(entry.path(), &target).unwrap();
            }
        }
    }
}

/// A repository built by stock git, then copied for zvcs.
struct Fixture {
    root: PathBuf,
    stock: &'static str,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// Two commits — `a` changed, `sub/b` kept — on `main`.
    fn new(label: &str) -> Option<Fixture> {
        let stock = stock_git::stock_git()?;
        let root = std::env::temp_dir().join(format!("zvcs-fsck-typed-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("stock/sub")).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let fx = Fixture { root, stock };
        fx.ok(&["init", "-q", "-b", "main", "."], b"");
        std::fs::write(fx.dir().join("a"), "one\n").unwrap();
        std::fs::write(fx.dir().join("sub/b"), "two\n").unwrap();
        fx.ok(&["add", "."], b"");
        fx.ok(&["commit", "-q", "-m", "one"], b"");
        std::fs::write(fx.dir().join("a"), "one\nthree\n").unwrap();
        fx.ok(&["commit", "-q", "-am", "two"], b"");
        Some(fx)
    }

    fn dir(&self) -> PathBuf {
        self.root.join("stock")
    }

    #[track_caller]
    fn ok(&self, args: &[&str], stdin: &[u8]) -> String {
        let out = git(self.stock, &self.dir(), args, stdin);
        assert_eq!(out.code, 0, "stock git {args:?}: {}", out.stderr);
        out.stdout.trim_end().to_owned()
    }

    /// Write a raw object of `kind`, unchecked.
    fn object(&self, kind: &str, body: &[u8]) -> String {
        self.ok(&["hash-object", "-w", "--literally", "--stdin", "-t", kind], body)
    }

    /// A tree of `(mode, name, id)` entries, in the order given.
    fn tree(&self, entries: &[(&str, &str, &str)]) -> String {
        let mut body = Vec::new();
        for (mode, name, id) in entries {
            body.extend_from_slice(format!("{mode} {name}\0").as_bytes());
            body.extend((0..id.len()).step_by(2).map(|i| u8::from_str_radix(&id[i..i + 2], 16).unwrap()));
        }
        self.object("tree", &body)
    }

    fn commit(&self, tree: &str, parent: Option<&str>) -> String {
        let parent = parent.map(|p| format!("parent {p}\n")).unwrap_or_default();
        let body = format!(
            "tree {tree}\n{parent}author A <a@e> 1700000000 +0000\ncommitter C <c@e> 1700000000 +0000\n\nx\n"
        );
        self.object("commit", body.as_bytes())
    }

    fn tag(&self, object: &str, kind: &str, name: &str) -> String {
        let body = format!("object {object}\ntype {kind}\ntag {name}\ntagger T <t@e> 1700000000 +0000\n\nm\n");
        self.object("tag", body.as_bytes())
    }

    /// Point `name` at `id` by writing the loose ref, which `update-ref` refuses for an
    /// object it cannot parse.
    fn set_ref(&self, name: &str, id: &str) {
        let path = self.dir().join(".git").join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("{id}\n")).unwrap();
    }

    /// Run `fsck <args>` on a fresh copy for each binary and require the same stdout,
    /// stderr and exit code.
    #[track_caller]
    fn same_fsck(&self, args: &[&str]) -> Outcome {
        let [stock, zvcs] = [self.stock, ZVCS].map(|bin| {
            let copy = self.root.join("run");
            let _ = std::fs::remove_dir_all(&copy);
            copy_dir(&self.dir(), &copy);
            let mut full = vec!["fsck", "--no-progress"];
            full.extend_from_slice(args);
            let out = git(bin, &copy, &full, b"");
            let _ = std::fs::remove_dir_all(&copy);
            out
        });
        assert_eq!(zvcs, stock, "git fsck {args:?}: left is zvcs, right is stock");
        stock
    }
}

/// A tree entry `40000 x <blob>`: the unreachable tree's scan types the blob a tree first,
/// so the blob's own scan fails (`is a tree, not a blob` / `object corrupt or missing`), the
/// reachable tree's entry for it fails in the scan (`broken links`), in the index and in the
/// walk (the NULL `broken link from` pair), and the walk reports it `missing tree`. Under
/// `--connectivity-only` there is no scan: the index types it a blob first, and the
/// directory entry is what breaks.
#[test]
fn a_directory_entry_naming_a_blob_is_typed_by_the_first_lookup() {
    let Some(fx) = Fixture::new("tree") else { return };
    let blob = fx.ok(&["rev-parse", "HEAD:a"], b"");
    let tree = fx.tree(&[("40000", "x", &blob)]);
    let commit = fx.commit(&tree, None);
    fx.set_ref("refs/heads/db", &commit);

    let out = fx.same_fsck(&[]);
    assert_eq!(out.code, 3);
    assert!(out.stdout.ends_with(&format!("broken link from    blob unknown\nmissing tree {blob}\n")), "{}", out.stdout);
    assert!(out.stderr.starts_with(&format!(
        "error: object {blob} is a tree, not a blob\nerror: {blob}: object corrupt or missing\n"
    )));

    let out = fx.same_fsck(&["--connectivity-only"]);
    assert_eq!(out.stdout, format!("broken link from    tree {tree}\nbroken link from    tree unknown\n"));
    assert_eq!(out.stderr, format!("error: object {blob} is a blob, not a tree\n"));

    fx.same_fsck(&["--name-objects", "--unreachable"]);
    fx.same_fsck(&["--verbose", "--root", "--tags"]);
    fx.same_fsck(&["HEAD"]);
}

/// A tag whose `type` header disagrees with the object: `snapshot_ref()` has typed `main`'s
/// commit already, so the tag's `lookup_blob()` fails its parse (`bad tag pointer`), the ref
/// is an `invalid sha1 pointer`, and the scan parses it again and fails again. A tag typing
/// a blob as a commit leaves the walk of that "commit" to `repo_parse_commit()`'s `Object
/// <oid> not a commit` under `--connectivity-only`.
#[test]
fn a_tag_whose_type_header_disagrees_with_its_object_fails_its_parse() {
    let Some(fx) = Fixture::new("tag") else { return };
    let head = fx.ok(&["rev-parse", "HEAD"], b"");
    let wrong = fx.tag(&head, "blob", "wrongtype");
    fx.set_ref("refs/tags/wrongtype", &wrong);

    let out = fx.same_fsck(&[]);
    assert_eq!(out.code, 3);
    let parse = format!(
        "error: object {head} is a commit, not a blob\nerror: bad tag pointer to {head} in {wrong}\n"
    );
    assert!(out.stderr.starts_with(&format!("{parse}error: refs/tags/wrongtype: invalid sha1 pointer {wrong}\n")));
    assert!(out.stderr.contains(&format!("{parse}error: {wrong}: object corrupt or missing\n")));

    let out = fx.same_fsck(&["--connectivity-only", "--unreachable"]);
    assert_eq!(out.stdout, format!("unreachable tag {wrong}\n"));
    fx.same_fsck(&["--tags", "--root", "--lost-found"]);

    let blob = fx.ok(&["rev-parse", "HEAD:a"], b"");
    let as_commit = fx.tag(&blob, "commit", "tcb");
    fx.set_ref("refs/tags/tcb", &as_commit);
    let out = fx.same_fsck(&["--connectivity-only"]);
    assert!(out.stderr.contains(&format!("error: Object {blob} not a commit\n")), "{}", out.stderr);
    fx.same_fsck(&[]);
}

/// A commit naming a tree as its parent fails `parse_commit_buffer()` with `bad parent`,
/// and an index entry naming a tree is dropped by `fsck_index()` after the type error.
#[test]
fn a_parent_and_an_index_entry_of_the_wrong_type_are_refused() {
    let Some(fx) = Fixture::new("parent") else { return };
    let tree = fx.ok(&["rev-parse", "HEAD^{tree}"], b"");
    let sub = fx.ok(&["rev-parse", "HEAD:sub"], b"");
    let bad = fx.commit(&tree, Some(&tree));
    fx.set_ref("refs/heads/cpt", &bad);
    fx.ok(&["update-index", "--add", "--cacheinfo", &format!("100644,{sub},zz")], b"");

    let out = fx.same_fsck(&[]);
    assert!(out.stderr.starts_with(&format!(
        "error: object {tree} is a tree, not a commit\nerror: bad parent {tree} in commit {bad}\n\
         error: refs/heads/cpt: invalid sha1 pointer {bad}\n"
    )));
    assert!(out.stderr.contains(&format!("error: object {sub} is a tree, not a blob\n")));
    fx.same_fsck(&["--connectivity-only", "--dangling"]);
    fx.same_fsck(&["--cache", "--no-reflogs", "--unreachable"]);
}
