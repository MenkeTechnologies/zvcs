//! `rev-list --objects-edge` / `--objects-edge-aggressive`: the `-<id>` lines
//! `mark_edges_uninteresting()` (list-objects.c:283-335) prints ahead of the
//! object listing.
//!
//! The three things a from-scratch implementation gets wrong, each pinned here:
//! an edge is any parent carrying `UNINTERESTING`, which `mark_parents_uninteresting()`
//! has painted over the *whole* ancestry of a `^rev` rather than over its tips
//! alone; the edge list is computed from `revs->commits` before `get_revision()`
//! runs, so `--max-count` and `--no-merges` cannot shrink it; and the aggressive
//! spelling adds a pass of its own over `revs->cmdline`, which holds the object
//! as *named*, so an annotated tag operand contributes nothing.
//!
//! Every expectation is the exact listing, in order, built from the ids
//! `rev-parse` reports.

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

/// ```text
/// a --- b --- c        (main, and `ta` is an annotated tag on a)
///        \
///         d --- e      (topic)
/// ```
///
/// `b` is the shape that matters: with `^main` excluded it is UNINTERESTING
/// without being the excluded tip, and it is the parent of a commit the walk
/// shows. Dates step by a minute per commit so the walk order is fixed.
fn fixture(tag: &str) -> Fixture {
    let dir = Fixture(
        std::env::temp_dir().join(format!("zvcs-revlist-edge-{tag}-{}", std::process::id())),
    );
    let _ = std::fs::remove_dir_all(dir.path());
    std::fs::create_dir_all(dir.path().join(".isolated-home")).unwrap();
    let repo = dir.path();
    ok(repo, &["init", "-q", "-b", "main"]);
    let mut commit = {
        let mut n = 0i64;
        move |repo: &Path, name: &str| {
            n += 1;
            std::fs::write(repo.join(format!("{name}.txt")), format!("{name}\n")).unwrap();
            let date = format!("{} +0000", 1_600_000_000 + n * 60);
            let out = cmd(repo, &["add", "-A"]).output().unwrap();
            assert!(out.status.success());
            let out = cmd(repo, &["commit", "-q", "-m", name])
                .env("GIT_AUTHOR_DATE", &date)
                .env("GIT_COMMITTER_DATE", &date)
                .output()
                .unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        }
    };
    commit(repo, "a");
    commit(repo, "b");
    commit(repo, "c");
    ok(repo, &["checkout", "-q", "-b", "topic", "main~1"]);
    commit(repo, "d");
    commit(repo, "e");
    ok(repo, &["checkout", "-q", "main"]);
    ok(repo, &["tag", "-a", "ta", "-m", "ta", "main~2"]);
    dir
}

/// The objects `^main topic` lists, with no edge lines: shared by the cases
/// below so each assertion is about the `-<id>` block alone.
fn topic_objects(repo: &Path) -> String {
    format!(
        "{e}\n{d}\n{tree_e} \n{d_txt} d.txt\n{e_txt} e.txt\n{tree_d} \n",
        e = oid(repo, "topic"),
        d = oid(repo, "topic~1"),
        tree_e = oid(repo, "topic^{tree}"),
        d_txt = oid(repo, "topic:d.txt"),
        e_txt = oid(repo, "topic:e.txt"),
        tree_d = oid(repo, "topic~1^{tree}"),
    )
}

/// `b` is neither the excluded tip nor a shown commit — it is UNINTERESTING only
/// because `mark_parents_uninteresting()` painted it while walking down from
/// `^main`. Testing the excluded tips alone finds no edge at all here.
#[test]
fn an_edge_is_any_uninteresting_parent_not_just_an_excluded_tip() {
    let dir = fixture("closure");
    let repo = dir.path();
    let out = ok(repo, &["rev-list", "--objects", "--objects-edge", "^main", "topic"]);
    assert_eq!(out, format!("-{}\n{}", oid(repo, "main~1"), topic_objects(repo)));
}

/// `mark_edges_uninteresting()` runs in `cmd_rev_list()` before
/// `traverse_commit_list()`, over the list `prepare_revision_walk()` left
/// behind. `--max-count` and `--no-merges` are spent inside `get_revision()`
/// afterwards, so neither can take an edge away — `-n 1` still reports `b` even
/// though the commit whose parent it is was never printed.
#[test]
fn output_filters_do_not_shrink_the_edge_list() {
    let dir = fixture("filters");
    let repo = dir.path();
    let edge = format!("-{}\n", oid(repo, "main~1"));

    let out = ok(repo, &["rev-list", "--objects", "--objects-edge", "^main", "topic", "-n", "1"]);
    assert_eq!(
        out,
        format!(
            "{edge}{e}\n{tree_e} \n{d_txt} d.txt\n{e_txt} e.txt\n",
            e = oid(repo, "topic"),
            tree_e = oid(repo, "topic^{tree}"),
            d_txt = oid(repo, "topic:d.txt"),
            e_txt = oid(repo, "topic:e.txt"),
        )
    );

    let out = ok(
        repo,
        &["rev-list", "--objects", "--objects-edge", "^main", "--no-merges", "topic"],
    );
    assert_eq!(out, format!("{edge}{}", topic_objects(repo)));
}

/// The aggressive spelling adds `revs->cmdline`'s excluded commits after the
/// parent pass, in the order they were given: `^main` then `^main~2`. The plain
/// spelling reports neither, because neither borders a shown commit.
#[test]
fn aggressive_adds_every_excluded_command_line_commit() {
    let dir = fixture("aggressive");
    let repo = dir.path();
    let out = ok(
        repo,
        &["rev-list", "--objects", "--objects-edge-aggressive", "^main", "^main~2", "topic"],
    );
    assert_eq!(
        out,
        format!(
            "-{b}\n-{c}\n-{a}\n{objects}",
            b = oid(repo, "main~1"),
            c = oid(repo, "main"),
            a = oid(repo, "main~2"),
            objects = topic_objects(repo),
        )
    );

    let out = ok(
        repo,
        &["rev-list", "--objects", "--objects-edge", "^main", "^main~2", "topic"],
    );
    assert_eq!(out, format!("-{}\n{}", oid(repo, "main~1"), topic_objects(repo)));
}

/// `add_rev_cmdline()` stores the object `get_reference()` answered with, before
/// `handle_commit()` peels it, and the aggressive pass skips anything that is not
/// an `OBJ_COMMIT`. `^ta` and `^main~2` name the same commit; only the raw
/// spelling becomes an edge.
#[test]
fn an_annotated_tag_operand_is_not_an_aggressive_edge() {
    let dir = fixture("tag-operand");
    let repo = dir.path();
    assert_eq!(oid(repo, "ta^{}"), oid(repo, "main~2"));
    let out = ok(
        repo,
        &["rev-list", "--objects", "--objects-edge-aggressive", "^main", "^ta", "topic"],
    );
    assert_eq!(
        out,
        format!(
            "-{b}\n-{c}\n{objects}",
            b = oid(repo, "main~1"),
            c = oid(repo, "main"),
            objects = topic_objects(repo),
        )
    );
}

/// `if (revs->tag_objects && !(flags & UNINTERESTING)) add_pending_object(...)`
/// (revision.c:396): an excluded tag operand is peeled for the walk but never
/// listed. The commit it names is excluded with it, while `b` — outside that
/// tag's ancestry — is still listed with everything under it.
#[test]
fn an_excluded_tag_operand_lists_no_tag_object() {
    let dir = fixture("excluded-tag");
    let repo = dir.path();
    let out = ok(repo, &["rev-list", "--objects", "^ta", "topic"]);
    assert!(
        !out.contains(&oid(repo, "ta")),
        "the excluded tag object was listed:\n{out}"
    );
    assert_eq!(
        out,
        format!(
            "{e}\n{d}\n{b}\n{tree_e} \n{b_txt} b.txt\n{d_txt} d.txt\n{e_txt} e.txt\n{tree_d} \n{tree_b} \n",
            e = oid(repo, "topic"),
            d = oid(repo, "topic~1"),
            b = oid(repo, "main~1"),
            tree_e = oid(repo, "topic^{tree}"),
            b_txt = oid(repo, "main~1:b.txt"),
            d_txt = oid(repo, "topic:d.txt"),
            e_txt = oid(repo, "topic:e.txt"),
            tree_d = oid(repo, "topic~1^{tree}"),
            tree_b = oid(repo, "main~1^{tree}"),
        )
    );
}
