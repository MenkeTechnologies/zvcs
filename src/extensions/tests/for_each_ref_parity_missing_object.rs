//! A ref whose object is missing dies where git's ref-filter reaches it.
//!
//! `apply_ref_filter()` does not open the object — "We do not open the object
//! yet; sort may only need refname to do its job and the resulting list may yet
//! to be pruned by maxcount logic" (ref-filter.c:3002-3006). The absence becomes
//! `missing object %s for %s` only inside `populate_value()` → `get_object()`
//! (ref-filter.c:2359-2361), which runs when the ref is formatted or first
//! compared by the sort. With the default refname order,
//! `filter_and_format_refs()` formats each ref as it iterates
//! (`can_do_iterative_format()`, ref-filter.c:3385-3445;
//! `filter_and_format_one()`, ref-filter.c:3061-3093): every earlier line is
//! already on stdout, and a `--count` that is reached first stops the iteration
//! before the broken ref is ever looked at. A sorted listing fills every ref
//! first, so it dies with no output. `git tag` lists through the same function
//! (builtin/tag.c:75).
//!
//! zvcs read every object while collecting refs: `for-each-ref` died before the
//! first line and ignored `--count`, and `git tag` printed a `zvcs:` gix error at
//! exit 1 instead of the fatal.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");
const MISSING: &str = "0123456789012345678901234567890123456789";

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
    /// `main` at `two`; `light` and annotated `ann` at `one`; `zz` at `two`; and
    /// `refs/tags/missing`, a loose ref naming an object that does not exist, which
    /// sorts between `light` and `zz`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-fer-missing-object-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "one\n").unwrap();
        f.run(&["add", "file"]);
        f.run(&["commit", "-q", "-m", "one"]);
        f.run(&["tag", "light"]);
        f.run(&["tag", "-a", "-m", "ann", "ann"]);
        std::fs::write(f.work.join("file"), "two\n").unwrap();
        f.run(&["commit", "-q", "-am", "two"]);
        f.run(&["tag", "zz"]);
        std::fs::write(f.work.join(".git/refs/tags/missing"), format!("{MISSING}\n")).unwrap();
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

    fn oid(&self, rev: &str) -> String {
        self.run(&["rev-parse", rev]).0.trim().to_string()
    }
}

fn died() -> String {
    format!("fatal: missing object {MISSING} for refs/tags/missing\n")
}

#[test]
fn default_listing_prints_the_refs_before_the_missing_one() {
    let f = Fixture::new("default");
    let want = format!(
        "{} commit\trefs/heads/main\n{} tag\trefs/tags/ann\n{} commit\trefs/tags/light\n",
        f.oid("main"),
        f.oid("refs/tags/ann"),
        f.oid("light"),
    );
    let (out, err, code) = f.run(&["for-each-ref"]);
    assert_eq!((out.as_str(), err.as_str(), code), (want.as_str(), died().as_str(), 128));
}

#[test]
fn count_reached_before_the_missing_ref_never_reads_it() {
    let f = Fixture::new("count");
    let fmt = "--format=%(objecttype)";
    assert_eq!(
        f.run(&["for-each-ref", "--count=3", fmt]),
        ("commit\ntag\ncommit\n".into(), String::new(), 0)
    );
    assert_eq!(
        f.run(&["for-each-ref", "--count=4", fmt]),
        ("commit\ntag\ncommit\n".into(), died(), 128)
    );
}

#[test]
fn a_sorted_listing_dies_before_any_output() {
    let f = Fixture::new("sorted");
    assert_eq!(
        f.run(&["for-each-ref", "--sort=-refname", "--format=%(objecttype)"]),
        (String::new(), died(), 128)
    );
    assert_eq!(
        f.run(&["tag", "--sort=-refname", "--format=%(objecttype)"]),
        (String::new(), died(), 128)
    );
}

#[test]
fn tag_listing_streams_and_dies_with_the_fatal() {
    let f = Fixture::new("tag");
    assert_eq!(
        f.run(&["tag", "--format=%(objecttype)"]),
        ("tag\ncommit\n".into(), died(), 128)
    );
    assert_eq!(f.run(&["tag", "-l", "m*", "--format=%(objecttype)"]), (String::new(), died(), 128));
}

#[test]
fn a_format_that_never_opens_the_object_lists_the_ref() {
    let f = Fixture::new("names");
    assert_eq!(
        f.run(&["for-each-ref", "--format=%(refname) %(objectname:short=7)", "refs/tags/m*"]),
        ("refs/tags/missing 0123456\n".into(), String::new(), 0)
    );
    assert_eq!(f.run(&["tag"]), ("ann\nlight\nmissing\nzz\n".into(), String::new(), 0));
}
