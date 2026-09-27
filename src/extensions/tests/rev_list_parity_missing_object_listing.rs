//! `rev-list --objects` over a missing object: the words and what stands on stdout.
//!
//! - `finish_object__ma()` dies `missing %s object '%s'` with the type name
//!   (builtin/rev-list.c:201-204); zvcs said `missing object '<id>'`.
//! - A tree that does not parse dies earlier, in `process_tree()` itself:
//!   `die("bad tree object %s")` unless a `--missing=` action set
//!   `do_not_die_on_missing_objects` (list-objects.c:173-187).
//! - `traverse_commit_list()` prints as it goes — every commit, then the
//!   trees and blobs — so the listing up to the missing object has been
//!   written when the `die()` fires. zvcs buffered it and printed nothing.
//!   `--count` is a summary printed after the walk, so it stays empty.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    work: PathBuf,
    /// The id of the object `new()` deleted.
    gone: String,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// A adds `a` and `d/x`; B adds `b`. Then the loose object `spec` names is deleted.
    fn new(tag: &str, spec: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-rev-list-missing-listing-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(work.join("d")).unwrap();
        let mut f = Fixture { root, work, gone: String::new() };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        std::fs::write(f.work.join("d/x"), "x\n").unwrap();
        f.run(&["add", "."]);
        f.run(&["commit", "-q", "-m", "A"]);
        std::fs::write(f.work.join("b"), "b\n").unwrap();
        f.run(&["add", "b"]);
        f.run(&["commit", "-q", "-m", "B"]);
        let id = f.rev(spec);
        std::fs::remove_file(f.work.join(".git/objects").join(&id[..2]).join(&id[2..])).unwrap();
        f.gone = id;
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn rev(&self, spec: &str) -> String {
        self.run(&["rev-parse", spec]).0.trim_end().to_string()
    }
}

#[test]
fn a_missing_blob_names_its_type_after_the_listing_so_far() {
    let f = Fixture::new("blob", "main:b");
    let (b, a, tree, blob_a) = (f.rev("main"), f.rev("main~1"), f.rev("main^{tree}"), f.rev("main:a"));
    let err = format!("fatal: missing blob object '{}'\n", f.gone);

    let out = f.run(&["rev-list", "--objects", "main"]);
    assert_eq!(out, (format!("{b}\n{a}\n{tree} \n{blob_a} a\n"), err.clone(), 128));

    let out = f.run(&["rev-list", "--objects", "--in-commit-order", "main"]);
    assert_eq!(out, (format!("{b}\n{tree} \n{blob_a} a\n"), err.clone(), 128));

    let out = f.run(&["rev-list", "--objects", "--count", "main"]);
    assert_eq!(out, (String::new(), err, 128));
}

#[test]
fn a_missing_tree_is_a_bad_tree_object() {
    let f = Fixture::new("tree", "main:d");
    let (b, a, tree) = (f.rev("main"), f.rev("main~1"), f.rev("main^{tree}"));
    let out = f.run(&["rev-list", "--objects", "main"]);
    assert_eq!(
        out,
        (
            format!("{b}\n{a}\n{tree} \n{} a\n{} b\n", f.rev("main:a"), f.rev("main:b")),
            format!("fatal: bad tree object {}\n", f.gone),
            128
        )
    );
}
