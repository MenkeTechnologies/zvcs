//! `rev-list --objects --filter=<spec>`: the filters `list-objects-filter.c`
//! applies during the walk, and the diagnostics `list-objects-filter-options.c`
//! dies with.
//!
//! Every expectation is built from the ids `rev-parse` reports, so the checks are
//! on the exact listing — which objects, in which order, under which path —
//! rather than on line counts that a wrong walk can also satisfy.

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");
const FIXED_DATE: &str = "2026-01-01T00:00:00+00:00";

fn cmd(repo: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(BIN);
    c.args(args)
        .current_dir(repo)
        .env("HOME", repo.join(".isolated-home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", FIXED_DATE)
        .env("GIT_COMMITTER_DATE", FIXED_DATE);
    c
}

fn run(repo: &Path, args: &[&str]) -> Output {
    cmd(repo, args).output().unwrap()
}

fn ok(repo: &Path, args: &[&str]) -> String {
    let out = run(repo, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn oid(repo: &Path, rev: &str) -> String {
    ok(repo, &["rev-parse", rev]).trim().to_string()
}

/// A scratch repository, removed again when the test ends.
struct Fixture(std::path::PathBuf);

impl Fixture {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Two commits: the second changes `src/lib.rs`, so `src` and the root tree
/// differ between them while `src/deep` is shared. `sparse` holds a
/// sparse-checkout specification that takes `src/` in and `src/deep/` out.
fn fixture(tag: &str) -> Fixture {
    let dir = Fixture(std::env::temp_dir().join(format!("zvcs-revlist-filter-{tag}-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(dir.path());
    std::fs::create_dir_all(dir.path().join(".isolated-home")).unwrap();
    let repo = dir.path();
    ok(repo, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(repo.join("src/deep")).unwrap();
    std::fs::write(repo.join("README.md"), "hi\n").unwrap();
    std::fs::write(repo.join("sparse"), "src/\n!src/deep/\n").unwrap();
    std::fs::write(repo.join("src/lib.rs"), "a\n").unwrap();
    std::fs::write(repo.join("src/deep/x.txt"), "b\n").unwrap();
    ok(repo, &["add", "README.md", "sparse", "src"]);
    ok(repo, &["commit", "-q", "-m", "one"]);
    std::fs::write(repo.join("src/lib.rs"), "a2\n").unwrap();
    ok(repo, &["add", "src/lib.rs"]);
    ok(repo, &["commit", "-q", "-m", "two"]);
    dir
}

fn lines(entries: &[(String, &str)]) -> String {
    entries
        .iter()
        .map(|(id, name)| match name {
            &"-" => format!("{id}\n"),
            name => format!("{id} {name}\n"),
        })
        .collect()
}

/// A second `--filter` is combined with the first (`transform_to_combine_type`),
/// so `tree:0` still removes every tree. Keeping only the last spec listed the
/// trees `blob:none` lets through.
#[test]
fn repeated_filters_are_combined_rather_than_replaced() {
    let dir = fixture("combined");
    let repo = dir.path();
    let out = ok(repo, &["rev-list", "--objects", "--filter=tree:0", "--filter=blob:none", "HEAD"]);
    assert_eq!(out, lines(&[(oid(repo, "HEAD"), "-"), (oid(repo, "HEAD~1"), "-")]));
}

/// `object:type=blob` also answers `LOFS_COMMIT`: the parent commit is left out,
/// while the commit named on the command line is exempt as `USER_GIVEN` — until
/// `--filter-provided-objects` takes that exemption away.
#[test]
fn object_type_filters_commits_that_were_not_named() {
    let dir = fixture("object-type");
    let repo = dir.path();
    let blobs = [
        (oid(repo, "HEAD:README.md"), "README.md"),
        (oid(repo, "HEAD:sparse"), "sparse"),
        (oid(repo, "HEAD:src/deep/x.txt"), "src/deep/x.txt"),
        (oid(repo, "HEAD:src/lib.rs"), "src/lib.rs"),
        (oid(repo, "HEAD~1:src/lib.rs"), "src/lib.rs"),
    ];

    let out = ok(repo, &["rev-list", "--objects", "--filter=object:type=blob", "HEAD"]);
    let mut expected = vec![(oid(repo, "HEAD"), "-")];
    expected.extend(blobs.iter().cloned());
    assert_eq!(out, lines(&expected));

    let out = ok(
        repo,
        &["rev-list", "--objects", "--filter=object:type=blob", "--filter-provided-objects", "HEAD"],
    );
    assert_eq!(out, lines(&blobs));
}

/// `sparse:oid=` shows every tree once, and a blob only where the specification
/// (or the directory it sits in) matches its path. `src/deep` is shared by both
/// commits and holds a provisionally omitted blob, so it is walked again for the
/// second commit without being listed twice.
#[test]
fn sparse_oid_lists_the_blobs_the_specification_takes_in() {
    let dir = fixture("sparse");
    let repo = dir.path();
    let out = ok(repo, &["rev-list", "--objects", "--filter=sparse:oid=HEAD:sparse", "HEAD"]);
    assert_eq!(
        out,
        lines(&[
            (oid(repo, "HEAD"), "-"),
            (oid(repo, "HEAD~1"), "-"),
            (oid(repo, "HEAD^{tree}"), ""),
            (oid(repo, "HEAD:src"), "src"),
            (oid(repo, "HEAD:src/deep"), "src/deep"),
            (oid(repo, "HEAD:src/lib.rs"), "src/lib.rs"),
            (oid(repo, "HEAD~1^{tree}"), ""),
            (oid(repo, "HEAD~1:src"), "src"),
            (oid(repo, "HEAD~1:src/lib.rs"), "src/lib.rs"),
        ])
    );
}

/// The parser's own diagnostics: a bad sub-filter is named on its own, and a
/// sparse specification that does not resolve dies when the filter is built.
#[test]
fn filter_diagnostics_are_the_parsers_and_initialisers() {
    let dir = fixture("diagnostics");
    let repo = dir.path();
    for (spec, message) in [
        ("combine:blob:none+bogus:spec", "fatal: invalid filter-spec 'bogus:spec'\n"),
        ("object:type=bogus", "fatal: 'bogus' for 'object:type=<type>' is not a valid object type\n"),
        ("tree:notanumber", "fatal: expected 'tree:<depth>'\n"),
        ("sparse:oid=HEAD:nosuch", "fatal: unable to access sparse blob in 'HEAD:nosuch'\n"),
    ] {
        let arg = format!("--filter={spec}");
        let out = run(repo, &["rev-list", "--objects", &arg, "HEAD"]);
        assert_eq!(out.status.code(), Some(128), "{spec}");
        assert!(out.stdout.is_empty(), "{spec}");
        assert_eq!(String::from_utf8_lossy(&out.stderr), message, "{spec}");
    }
}
