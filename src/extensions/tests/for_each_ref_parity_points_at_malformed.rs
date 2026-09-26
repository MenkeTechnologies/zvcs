//! `--points-at` dies on a ref whose object cannot be parsed.
//!
//! `match_points_at()` (ref-filter.c:2840-2866) answers yes at once when the
//! ref's own id was asked for; otherwise it `parse_object()`s the ref and walks
//! its tag chain, and when an object it has to parse is not there it ends in
//! `die(_("malformed object at '%s'"), refname)`. It is the first test
//! `apply_ref_filter()` applies after the name patterns (ref-filter.c:2979-2980),
//! so it fires even when `--contains` would drop the ref. A tag's target is only
//! looked up, typed by the tag's `type` header, so a tag whose target commit is
//! missing is not malformed — it matches when its target was asked for.
//!
//! The die is raised mid-walk: with the default refname order
//! `filter_and_format_refs()` formats each ref as it goes (ref-filter.c:3419-3435),
//! so the refs before the malformed one are already on stdout and a `--count`
//! reached first never gets to it; a sorted listing dies with nothing written.
//! `git tag` lists through the same function after verifying its format
//! (builtin/tag.c:72-75).
//!
//! zvcs peeled every ref up front with an error-propagating walk and aborted with
//! a `zvcs:` gix error at exit 1, printing nothing — and did so even for
//! `--points-at <the missing id>`, which stock answers by listing the ref.
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
            .join(format!("zvcs-fer-points-at-malformed-{tag}-{}", std::process::id()));
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

const MISSING: &str = "0123456789012345678901234567890123456789";
const GONE_TARGET: &str = "1111111111111111111111111111111111111111";

fn died() -> String {
    "fatal: malformed object at 'refs/tags/missing'\n".to_string()
}

#[test]
fn for_each_ref_writes_the_refs_before_the_malformed_one() {
    let f = Fixture::new("fer");
    let fmt = "--format=%(refname)";
    assert_eq!(
        f.run(&["for-each-ref", "--points-at", "light", fmt]),
        ("refs/tags/ann\nrefs/tags/light\n".into(), died(), 128)
    );
    // `--count` is met before the walk reaches `refs/tags/missing`.
    assert_eq!(
        f.run(&["for-each-ref", "--points-at", "light", "--count=2", fmt]),
        ("refs/tags/ann\nrefs/tags/light\n".into(), String::new(), 0)
    );
    // Sorted: `filter_refs()` runs whole before anything is written.
    assert_eq!(
        f.run(&["for-each-ref", "--points-at", "light", "--sort=-refname", fmt]),
        (String::new(), died(), 128)
    );
    // The points-at test comes before the reachability filters.
    assert_eq!(
        f.run(&["for-each-ref", "--points-at", "light", "--contains", "main", fmt]),
        (String::new(), died(), 128)
    );
}

#[test]
fn asking_for_the_unreadable_id_itself_matches_without_parsing() {
    let f = Fixture::new("own-id");
    let fmt = "--format=%(refname)";
    assert_eq!(
        f.run(&["for-each-ref", "--points-at", MISSING, fmt]),
        ("refs/tags/missing\n".into(), String::new(), 0)
    );
    assert_eq!(f.run(&["tag", "--points-at", MISSING]), ("missing\n".into(), String::new(), 0));
    // The dangling tag parses; only its target is gone, and that target is
    // what matches.
    assert_eq!(
        f.run(&["for-each-ref", "--points-at", GONE_TARGET, fmt]),
        ("refs/tags/dangling\n".into(), died(), 128)
    );
}

#[test]
fn tag_points_at_streams_then_dies() {
    let f = Fixture::new("tag");
    assert_eq!(f.run(&["tag", "--points-at", "light"]), ("ann\nlight\n".into(), died(), 128));
    assert_eq!(
        f.run(&["tag", "--points-at", "light", "--sort=-refname"]),
        (String::new(), died(), 128)
    );
    // A format error is reported before the walk starts.
    assert_eq!(
        f.run(&["tag", "--points-at", "light", "--format=%(bogus)"]),
        (String::new(), "fatal: unknown field name: bogus\n".into(), 128)
    );
}
