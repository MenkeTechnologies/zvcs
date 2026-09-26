//! A `*`-atom on a tag whose target is missing dies with `bad tag`.
//!
//! `populate_value()` fills the tag's own atoms through `get_object()` first,
//! then, when any `*`-atom is in use, peels it:
//!
//! ```c
//! if (!is_null_oid(&ref->peeled_oid)) {
//!         oidcpy(&oi_deref.oid, &ref->peeled_oid);
//! } else if (!peel_object(the_repository, &oi.oid, &oi_deref.oid,
//!                         PEEL_OBJECT_VERIFY_TAGGED_OBJECT_TYPE)) {
//!         /* We managed to peel the object ourselves. */
//! } else {
//!         die("bad tag");
//! }
//! ```
//!
//! (ref-filter.c:2634-2642.) The verify flag reads each target's type
//! (object.c:235-239), so a missing target is `PEEL_INVALID`. Like every
//! `populate_value()` failure it happens when the ref is first sorted or
//! formatted: an iterative listing has written the refs before it and a
//! `--count` reached first never meets it (ref-filter.c:3061-3093); a sorted
//! one dies with nothing written. The tag's own date formats are grabbed before
//! the peel, so `%(taggerdate:bogus)` fails first.
//!
//! zvcs peeled while collecting refs and aborted every such listing with a
//! `zvcs:` gix error at exit 1 and no output.
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
    /// `main` at `two`; `light` and annotated `ann` at `one`; `zz` at `two`;
    /// and `refs/tags/dangling`, a real tag object whose target commit does not
    /// exist.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-fer-bad-tag-{tag}-{}", std::process::id()));
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
        let body = "object 1111111111111111111111111111111111111111\ntype commit\ntag dangling\n\
                    tagger C <c@example.com> 1700000000 +0000\n\ndangling\n";
        std::fs::write(f.root.join("tag-body"), body).unwrap();
        let tag_path = f.root.join("tag-body");
        let (oid, _, code) = f.run(&["hash-object", "-t", "tag", "-w", tag_path.to_str().unwrap()]);
        assert_eq!(code, 0);
        f.run(&["update-ref", "refs/tags/dangling", oid.trim()]);
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
}

fn bad_tag() -> String {
    "fatal: bad tag\n".to_string()
}

#[test]
fn for_each_ref_writes_the_refs_before_the_bad_tag() {
    let f = Fixture::new("fer");
    let one = f.run(&["rev-parse", "light"]).0;
    let want = format!("refs/heads/main\nrefs/tags/ann {}", one);
    assert_eq!(
        f.run(&["for-each-ref", "--format=%(refname)%(if)%(*objectname)%(then) %(*objectname)%(end)"]),
        (want, bad_tag(), 128)
    );
    assert_eq!(
        f.run(&["for-each-ref", "--format=%(*refname)"]),
        ("refs/heads/main^{}\nrefs/tags/ann^{}\n".into(), bad_tag(), 128)
    );
    // `--count` stops the walk at `ann`, before `dangling`.
    assert_eq!(
        f.run(&["for-each-ref", "--count=2", "--format=%(*objecttype)"]),
        ("\ncommit\n".into(), String::new(), 0)
    );
    // A `*` sort key peels every ref before the first line.
    assert_eq!(
        f.run(&["for-each-ref", "--format=%(refname)", "--sort=*objectname"]),
        (String::new(), bad_tag(), 128)
    );
}

#[test]
fn the_tags_own_date_format_fails_before_the_peel() {
    let f = Fixture::new("order");
    let (out, err, code) =
        f.run(&["for-each-ref", "--format=%(taggerdate:bogus)%(*objectname)", "refs/tags/dangling"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "fatal: unknown date format bogus\n", 128));
    // An author date is a commit's, so the tag grabs none and the peel dies.
    assert_eq!(
        f.run(&["for-each-ref", "--format=%(authordate:bogus)%(*objectname)", "refs/tags/dangling"]),
        (String::new(), bad_tag(), 128)
    );
}

#[test]
fn tag_listing_dies_the_same_way() {
    let f = Fixture::new("tag");
    assert_eq!(f.run(&["tag", "--format=%(*objecttype)"]), ("commit\n".into(), bad_tag(), 128));
    assert_eq!(
        f.run(&["tag", "--sort=-refname", "--format=%(*objecttype)"]),
        (String::new(), bad_tag(), 128)
    );
    assert_eq!(f.run(&["tag", "--sort=*objecttype"]), (String::new(), bad_tag(), 128));
}
