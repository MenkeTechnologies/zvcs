//! `fast-export` of revisions that are not commits: tags of blobs and trees,
//! and blobs or trees named directly.
//!
//! `handle_revision_arg()` pends whatever object a name resolves to; the walk
//! drops a blob or tree because `revs->blob_objects` / `tree_objects` are off.
//! `get_tags_and_duplicates()` then peels each named tag through
//! `get_commit()` and, for a chain that ends in a blob, calls `export_blob()`
//! at once, ahead of every commit (builtin/fast-export.c:1090-1093); a tree
//! there is `tag points to object of unexpected type tree, skipping.`
//! (:1094-1098), and a ref naming a blob directly is `<name>: unexpected
//! object of type blob, skipping.` (:1079-1085). `handle_tag()` later writes
//! the blob's tag `from :<blob mark>`, and omits a tree's with its own warning
//! (:899-908). `A...B` with a non-commit endpoint reports
//! `object <id> is a <type>, not a commit` (commit.c:61-66) and dies
//! `Invalid symmetric difference expression` (revision.c:2092-2095, :2046-2048).
//! zvcs refused every such name as an ambiguous argument.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

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
    /// `a` in the first commit, `bin` added by the second; `btag` tags the
    /// `bin` blob and `ttag` the second commit's tree.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-fast-export-blob-tags-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "one"]);
        std::fs::write(f.work.join("bin"), "bin").unwrap();
        f.run(&["add", "bin"]);
        f.run(&["commit", "-q", "-m", "bin"]);
        f.run(&["tag", "-a", "-m", "blobtag", "btag", "HEAD:bin"]);
        f.run(&["tag", "-a", "-m", "treetag", "ttag", "HEAD^{tree}"]);
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

    fn id(&self, spec: &str) -> String {
        self.run(&["rev-parse", spec]).0.trim_end().to_string()
    }
}

const BLOB_TAG: &str =
    "tag btag\nfrom :1\ntagger C O Mitter <committer@example.com> 1700000000 +0000\ndata 8\nblobtag\n\n";

#[test]
fn a_tag_of_a_blob_exports_the_blob_then_the_tag() {
    let f = Fixture::new("blob");
    let (out, err, code) = f.run(&["fast-export", "btag"]);
    let want = format!("blob\nmark :1\ndata 3\nbin\n{BLOB_TAG}");
    assert_eq!((out.as_str(), err.as_str(), code), (want.as_str(), "", 0));
}

#[test]
fn the_tagged_blob_is_written_ahead_of_every_commit() {
    let f = Fixture::new("ahead");
    let (out, err, code) = f.run(&["fast-export", "main", "btag"]);
    let want = format!(
        "blob\nmark :1\ndata 3\nbin\n\
         blob\nmark :2\ndata 2\na\n\n\
         reset refs/heads/main\n\
         commit refs/heads/main\nmark :3\n\
         author A U Thor <author@example.com> 1700000000 +0000\n\
         committer C O Mitter <committer@example.com> 1700000000 +0000\n\
         data 4\none\nM 100644 :2 a\n\n\
         commit refs/heads/main\nmark :4\n\
         author A U Thor <author@example.com> 1700000000 +0000\n\
         committer C O Mitter <committer@example.com> 1700000000 +0000\n\
         data 4\nbin\nfrom :3\nM 100644 :1 bin\n\n\
         {BLOB_TAG}"
    );
    assert_eq!((out.as_str(), err.as_str(), code), (want.as_str(), "", 0));
}

#[test]
fn no_data_leaves_the_blob_tag_without_a_mark() {
    let f = Fixture::new("nodata");
    let btag = f.id("btag");
    let (out, err, code) = f.run(&["fast-export", "--no-data", "btag"]);
    let fatal = format!(
        "fatal: tag {btag} tags unexported object; use --tag-of-filtered-object=<mode> to handle it\n"
    );
    assert_eq!((out.as_str(), err.as_str(), code), ("", fatal.as_str(), 128));
}

#[test]
fn a_tag_of_a_tree_is_skipped_with_both_warnings() {
    let f = Fixture::new("tree");
    let ttag = f.id("ttag");
    let (out, err, code) = f.run(&["fast-export", "ttag"]);
    let want = format!(
        "warning: tag points to object of unexpected type tree, skipping.\n\
         warning: omitting tag {ttag},\n\
         since tags of trees (or tags of tags of trees, etc.) are not supported.\n"
    );
    assert_eq!((out.as_str(), err.as_str(), code), ("", want.as_str(), 0));
}

#[test]
fn a_blob_or_tree_named_directly_exports_nothing() {
    let f = Fixture::new("direct");
    for spec in ["HEAD:bin", "HEAD^{tree}"] {
        let (out, err, code) = f.run(&["fast-export", spec]);
        assert_eq!((out.as_str(), err.as_str(), code), ("", "", 0), "{spec}");
    }
    f.run(&["update-ref", "refs/tags/rawblob", "HEAD:bin"]);
    let (out, err, code) = f.run(&["fast-export", "rawblob"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "warning: rawblob: unexpected object of type blob, skipping.\n", 0)
    );
}

#[test]
fn a_symmetric_range_with_a_blob_end_is_invalid() {
    let f = Fixture::new("symmetric");
    let btag = f.id("btag");
    let (out, err, code) = f.run(&["fast-export", "btag...main"]);
    let want = format!(
        "error: object {btag} is a blob, not a commit\n\
         fatal: Invalid symmetric difference expression btag...main\n"
    );
    assert_eq!((out.as_str(), err.as_str(), code), ("", want.as_str(), 128));
}
