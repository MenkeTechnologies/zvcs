//! The reachability filters drop a ref whose commit cannot be read.
//!
//! `apply_ref_filter()` makes one object lookup for `--contains`,
//! `--no-contains`, `--merged` and `--no-merged`:
//!
//! ```c
//! commit = lookup_commit_reference_gently(the_repository, ref->oid, 1);
//! if (!commit)
//!         return NULL;
//! ```
//!
//! (ref-filter.c:2987-2991). It is quiet, and `deref_tag()` (tag.c:76-95) walks
//! the tag chain with `parse_object()`, so a ref at a missing object and a tag
//! whose target is missing both come back NULL and are left out of the listing.
//! zvcs peeled with an error-propagating walk and aborted the whole listing with
//! a `zvcs:` gix error at exit 1 — in `for-each-ref`, and in `git tag` for the tag
//! whose target is gone.
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
    /// `refs/tags/missing` naming an object that does not exist; and
    /// `refs/tags/dangling`, a real tag object whose target commit does not.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-fer-filter-unreadable-{tag}-{}", std::process::id()));
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
        std::fs::write(
            f.work.join(".git/refs/tags/missing"),
            "0123456789012345678901234567890123456789\n",
        )
        .unwrap();
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

fn ok(out: &str) -> (String, String, i32) {
    (out.to_string(), String::new(), 0)
}

#[test]
fn for_each_ref_filters_skip_both_unreadable_refs() {
    let f = Fixture::new("fer");
    let fmt = "--format=%(refname)";
    assert_eq!(
        f.run(&["for-each-ref", "--contains", "main", fmt]),
        ok("refs/heads/main\nrefs/tags/zz\n")
    );
    assert_eq!(
        f.run(&["for-each-ref", "--no-contains", "main", fmt]),
        ok("refs/tags/ann\nrefs/tags/light\n")
    );
    assert_eq!(
        f.run(&["for-each-ref", "--merged", "main", fmt]),
        ok("refs/heads/main\nrefs/tags/ann\nrefs/tags/light\nrefs/tags/zz\n")
    );
    assert_eq!(f.run(&["for-each-ref", "--no-merged", "main", fmt]), ok(""));
}

#[test]
fn tag_filters_skip_a_tag_whose_target_is_missing() {
    let f = Fixture::new("tag");
    assert_eq!(f.run(&["tag", "--contains", "main"]), ok("zz\n"));
    assert_eq!(f.run(&["tag", "--no-contains", "main"]), ok("ann\nlight\n"));
    assert_eq!(f.run(&["tag", "--merged", "main"]), ok("ann\nlight\nzz\n"));
    assert_eq!(f.run(&["tag", "--no-merged", "main"]), ok(""));
    // Without a filter nothing opens either object, and both are listed.
    assert_eq!(f.run(&["tag"]), ok("ann\ndangling\nlight\nmissing\nzz\n"));
}
