//! `rev-list -z` and `--unpacked`, the two options `cmd_rev_list()` advertises in
//! its own usage block.
//!
//! `-z` moves both terminators to NUL (builtin/rev-list.c:752-755), which changes
//! the record shape rather than only the separator: the `-`/`<`/`>` mark is
//! dropped, `--boundary` becomes a `boundary=yes` field, an object name becomes a
//! `path=` field that is left out entirely when the object has no name, and the
//! option refuses outright to combine with anything that would print something
//! unparseable.
//!
//! `--unpacked` is `has_object_pack()` twice over: `get_commit_action()`
//! (revision.c:4182) for commits and `show_object()` (list-objects.c:44-46) for
//! everything else — so it narrows `--count` and `--disk-usage` too, not just
//! the listing.

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn cmd(repo: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(BIN);
    c.args(args)
        .current_dir(repo)
        .env("HOME", repo.join(".isolated-home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com");
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

fn commit(repo: &Path, name: &str, n: i64) {
    std::fs::write(repo.join(format!("{name}.txt")), format!("{name}\n")).unwrap();
    let date = format!("{} +0000", 1_600_000_000 + n * 60);
    assert!(cmd(repo, &["add", "-A"]).output().unwrap().status.success());
    let out = cmd(repo, &["commit", "-q", "-m", name])
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

/// ```text
/// a --- b --- c   (main)
///        \
///         d --- e (topic)
/// ```
fn fixture(tag: &str) -> Fixture {
    let dir =
        Fixture(std::env::temp_dir().join(format!("zvcs-revlist-nul-{tag}-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(dir.path());
    std::fs::create_dir_all(dir.path().join(".isolated-home")).unwrap();
    let repo = dir.path();
    ok(repo, &["init", "-q", "-b", "main"]);
    commit(repo, "a", 1);
    commit(repo, "b", 2);
    commit(repo, "c", 3);
    ok(repo, &["checkout", "-q", "-b", "topic", "main~1"]);
    commit(repo, "d", 4);
    commit(repo, "e", 5);
    ok(repo, &["checkout", "-q", "main"]);
    dir
}

/// An object with a name gets a `path=` field; the root tree of each commit is
/// pended with an empty name and gets none at all, where the newline form still
/// prints the separating space.
#[test]
fn nul_records_name_the_path_field_and_omit_it_when_empty() {
    let dir = fixture("objects");
    let repo = dir.path();
    let out = ok(repo, &["rev-list", "-z", "--objects", "^main", "topic"]);
    assert_eq!(
        out,
        format!(
            "{e}\0{d}\0{tree_e}\0{d_txt}\0path=d.txt\0{e_txt}\0path=e.txt\0{tree_d}\0",
            e = oid(repo, "topic"),
            d = oid(repo, "topic~1"),
            tree_e = oid(repo, "topic^{tree}"),
            d_txt = oid(repo, "topic:d.txt"),
            e_txt = oid(repo, "topic:e.txt"),
            tree_d = oid(repo, "topic~1^{tree}"),
        )
    );

    // `--no-object-names` drops the field but not the record.
    let out = ok(
        repo,
        &["rev-list", "-z", "--objects", "--no-object-names", "^main", "topic"],
    );
    assert_eq!(
        out,
        format!(
            "{e}\0{d}\0{tree_e}\0{d_txt}\0{e_txt}\0{tree_d}\0",
            e = oid(repo, "topic"),
            d = oid(repo, "topic~1"),
            tree_e = oid(repo, "topic^{tree}"),
            d_txt = oid(repo, "topic:d.txt"),
            e_txt = oid(repo, "topic:e.txt"),
            tree_d = oid(repo, "topic~1^{tree}"),
        )
    );
}

/// `if (!line_term) { if (commit->object.flags & BOUNDARY) printf("%cboundary=yes", info_term); }`
/// (builtin/rev-list.c:284-287): the `-` that prefixes a boundary commit in the
/// newline form has nowhere to go once records are NUL-separated, so the fact is
/// reported behind the object name instead.
#[test]
fn a_boundary_commit_reports_itself_as_a_field_under_nul() {
    let dir = fixture("boundary");
    let repo = dir.path();
    let out = ok(repo, &["rev-list", "-z", "--boundary", "^main", "topic"]);
    assert_eq!(
        out,
        format!(
            "{e}\0{d}\0{b}\0boundary=yes\0",
            e = oid(repo, "topic"),
            d = oid(repo, "topic~1"),
            b = oid(repo, "main~1"),
        )
    );
    // The newline form keeps the `-` prefix and has no field.
    let out = ok(repo, &["rev-list", "--boundary", "^main", "topic"]);
    assert_eq!(
        out,
        format!(
            "{e}\n{d}\n-{b}\n",
            e = oid(repo, "topic"),
            d = oid(repo, "topic~1"),
            b = oid(repo, "main~1"),
        )
    );
}

/// The list at builtin/rev-list.c:876-882, one representative per family: a
/// commit body, an edge listing and a marked walk all print text `-z` cannot
/// delimit, so the combination dies rather than producing it.
#[test]
fn nul_refuses_the_options_it_cannot_delimit() {
    let dir = fixture("refuse");
    let repo = dir.path();
    for extra in [
        vec!["--header"],
        vec!["--pretty=oneline"],
        vec!["--timestamp"],
        vec!["--disk-usage"],
        vec!["--bisect"],
        vec!["--left-right"],
        vec!["--cherry-mark"],
        vec!["--objects", "--objects-edge"],
    ] {
        let mut args = vec!["rev-list", "-z"];
        args.extend(extra.iter().copied());
        args.push("main");
        let out = run(repo, &args);
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            "fatal: -z option used with unsupported option\n",
            "{args:?} was accepted",
        );
        assert_eq!(out.status.code(), Some(128), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?} printed {:?}", out.stdout);
    }

    // Everything not on that list keeps working.
    let out = ok(repo, &["rev-list", "-z", "--count", "main"]);
    assert_eq!(out, "3\n");
}

/// After a `gc` every object but the newest commit's lives in a pack. The commit
/// filter alone would still list the packed trees and blobs reachable from the
/// loose commit, and would count them.
#[test]
fn unpacked_filters_objects_as_well_as_commits() {
    let dir = fixture("unpacked");
    let repo = dir.path();
    ok(repo, &["gc", "-q", "--prune=now"]);
    commit(repo, "loose", 6);

    let listed = format!(
        "{c}\n{tree} \n{blob} loose.txt\n",
        c = oid(repo, "main"),
        tree = oid(repo, "main^{tree}"),
        blob = oid(repo, "main:loose.txt"),
    );
    assert_eq!(ok(repo, &["rev-list", "--unpacked", "--objects", "--all"]), listed);
    // The same three objects, and only those three, reach `--count`.
    assert_eq!(ok(repo, &["rev-list", "--unpacked", "--objects", "--count", "--all"]), "3\n");
    // Without the option the packed history comes back.
    let full = ok(repo, &["rev-list", "--objects", "--all"]);
    assert!(full.lines().count() > 3, "the fixture was not packed:\n{full}");

    // `--unpacked=<packfile>` was removed; the spelling is now fatal.
    let out = run(repo, &["rev-list", "--unpacked=x.pack", "--all"]);
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "fatal: --unpacked=<packfile> no longer supported\n"
    );
    assert_eq!(out.status.code(), Some(128));
}
