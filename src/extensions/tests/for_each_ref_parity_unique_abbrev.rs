//! Abbreviated ids in the ref listings are `repo_find_unique_abbrev()`.
//!
//! `do_grab_oid()` renders `%(objectname:short=<n>)` as
//! `repo_find_unique_abbrev(oid, n)` and `:short` as
//! `repo_find_unique_abbrev(oid, DEFAULT_ABBREV)` (ref-filter.c:1422-1437);
//! `show-ref --abbrev` (builtin/show-ref.c:47, 57) and the `Deleted tag … (was
//! …)` line (builtin/tag.c:134) use the same call. It widens the starting length
//! while another object shares the prefix, and an id the object database does not
//! hold has nothing to share it with, so it keeps the starting length
//! (object-name.c:586-600).
//!
//! zvcs cut `:short=<n>` at `n` without widening, and printed a missing object's
//! id whole for `:short`, `show-ref --abbrev` and `tag -d`.
//!
//! The two blobs `blob 173\n` and `blob 514\n` hash to `258359f…` and
//! `258328b…`: they share four hex digits and differ at the fifth.
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
    /// One commit on `main`; `refs/tags/b173` and `refs/tags/b514` at the two
    /// colliding blobs; `refs/tags/missing` at an object that does not exist; and
    /// `refs/tags/dangling`, a tag object whose target commit does not exist.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-fer-unique-abbrev-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "one\n").unwrap();
        f.run(&["add", "file"]);
        f.run(&["commit", "-q", "-m", "one"]);
        for n in ["173", "514"] {
            let path = f.root.join(format!("b{n}"));
            std::fs::write(&path, format!("blob {n}\n")).unwrap();
            let (oid, _, code) = f.run(&["hash-object", "-w", path.to_str().unwrap()]);
            assert_eq!(code, 0);
            f.run(&["update-ref", &format!("refs/tags/b{n}"), oid.trim()]);
        }
        std::fs::write(
            f.work.join(".git/refs/tags/missing"),
            "0123456789012345678901234567890123456789\n",
        )
        .unwrap();
        let body = "object 1111111111111111111111111111111111111111\ntype commit\ntag dangling\n\
                    tagger C <c@example.com> 1700000000 +0000\n\ndangling\n";
        let tag_path = f.root.join("tag-body");
        std::fs::write(&tag_path, body).unwrap();
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
fn short_with_a_length_widens_past_a_shared_prefix() {
    let f = Fixture::new("widen");
    assert_eq!(
        f.run(&["for-each-ref", "--format=%(objectname:short=4) %(refname)"]),
        ok("2d80 refs/heads/main\n25835 refs/tags/b173\n25832 refs/tags/b514\nfcdb refs/tags/dangling\n0123 refs/tags/missing\n")
    );
}

#[test]
fn a_missing_object_keeps_the_starting_length() {
    let f = Fixture::new("missing");
    assert_eq!(
        f.run(&["for-each-ref", "--format=%(objectname:short)", "refs/tags/missing"]),
        ok("0123456\n")
    );
    assert_eq!(
        f.run(&["-c", "core.abbrev=12", "for-each-ref", "--format=%(objectname:short)", "refs/tags/missing"]),
        ok("012345678901\n")
    );
    // `show-ref -d` peels the dangling tag to its missing target without
    // reading it, and abbreviates that id too.
    let tag = f.run(&["rev-parse", "refs/tags/dangling"]).0;
    let want = format!("{} refs/tags/dangling\n1111111 refs/tags/dangling^{{}}\n", &tag[..7]);
    assert_eq!(f.run(&["show-ref", "--abbrev", "-d", "dangling"]), ok(&want));
}

#[test]
fn tag_delete_abbreviates_through_the_same_call() {
    let f = Fixture::new("tag-d");
    assert_eq!(
        f.run(&["-c", "core.abbrev=4", "tag", "-d", "b173", "missing"]),
        ok("Deleted tag 'b173' (was 25835)\nDeleted tag 'missing' (was 0123)\n")
    );
}
