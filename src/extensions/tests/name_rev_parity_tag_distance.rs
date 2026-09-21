//! `git name-rev`, two tags, one commit: the nearer tag wins, not the older one.
//!
//! `is_better_name()`'s first arm decides every contest between two tag-derived
//! names (builtin/name-rev.c:122-124):
//!
//! ```c
//! /* If both are tags, we prefer the nearer one. */
//! if (from_tag && name->from_tag)
//!         return name_distance > new_distance;
//! ```
//!
//! Distance only. The tagger date does not enter it. git expresses its preference
//! for the older tag exactly once, in the *tip ordering* — `cmp_by_tag_and_age()`
//! (`:360-374`) sorts tags before non-tags and older tagger dates first, so the
//! older tag's walk runs first and keeps the commit on an exact tie (`is_better_name`
//! ends with "keep the current one if we cannot decide", `:141-142`). A *farther*
//! older tag still loses.
//!
//! "Farther" is measured by `effective_distance()` (`:108-111`), which charges
//! `MERGE_TRAVERSAL_WEIGHT` (65535) for any non-zero generation. That makes the two
//! ways of reaching a commit almost exactly equal, and it is the case that separates
//! the two rules:
//!
//!   * one first-parent hop down from a tag — generation 1, distance 1, so an
//!     effective distance of 65536, and a name of `<tag>~1`;
//!   * the second parent of a merge that a tag sits on — generation 0, distance
//!     65535, so an effective distance of 65535, and a name of `<tag>^2`.
//!
//! The merge side is nearer by one. So when the `~1` tag is the older of the two,
//! the two rules disagree, and only the distance rule agrees with stock git.
//!
//! `git describe --contains` is `cmd_name_rev()` with a fixed argument vector
//! (builtin/describe.c:710-748), so it has to give the same answer.
//!
//! Expectations were read off a differential run against stock git 2.55.0 in a
//! byte-identical fixture.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(repo: &Path, home: &Path, date: &str, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .env("LC_ALL", "C")
        .output()
        .unwrap()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn at(second: u32) -> String {
    format!("2005-04-07T15:16:{:02}+0000", 17 + second)
}

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-nrdist-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        let home = root.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let f = Fixture { root, repo, home };
        f.ok(0, &["init", "-q", "-b", "main", "."]);
        f
    }

    fn ok(&self, second: u32, args: &[&str]) -> Output {
        let out = git(&self.repo, &self.home, &at(second), args);
        assert!(out.status.success(), "`git {args:?}` failed: {}", stderr(&out));
        out
    }

    /// A commit at `second`, adding one file named after itself. A lightweight tag
    /// takes its commit's date as its tagger date (`taggerdate = commit->date`,
    /// builtin/name-rev.c:443-444), so the commit's second is also the tag's.
    fn commit(&self, second: u32, name: &str) -> String {
        std::fs::write(self.repo.join(name), format!("{name}\n")).unwrap();
        self.ok(second, &["add", name]);
        self.ok(second, &["commit", "-q", "-m", name]);
        self.rev("HEAD")
    }

    fn rev(&self, spec: &str) -> String {
        stdout(&self.ok(0, &["rev-parse", spec])).trim().to_owned()
    }

    fn run(&self, args: &[&str]) -> Output {
        git(&self.repo, &self.home, &at(0), args)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// ```text
///   root -- target -- near-old          (main)
///     \           \
///      other ----- far-young            (other, a merge of other and target)
/// ```
///
/// `target` is one first-parent hop below `near-old` and the *second* parent of
/// `far-young`. `near-old` is tagged at second 3 and `far-young` at second 4, so the
/// tip table walks `near-old` first and `target` is claimed as `near-old~1` before
/// `far-young` ever runs.
fn two_ways_to_reach_one_commit(name: &str) -> (Fixture, String) {
    let f = Fixture::new(name);
    let root = f.commit(0, "f0");
    let target = f.commit(1, "f1");

    f.ok(0, &["checkout", "-q", "-b", "other", &root]);
    f.commit(2, "g0");

    f.ok(0, &["checkout", "-q", "main"]);
    f.commit(3, "f2");
    f.ok(0, &["tag", "near-old"]);

    f.ok(0, &["checkout", "-q", "other"]);
    f.ok(4, &["merge", "-q", "--no-ff", "-m", "merge", &target]);
    f.ok(0, &["tag", "far-young"]);

    (f, target)
}

/// The younger tag reaches `target` across a merge — generation 0, distance 65535 —
/// and the older one reaches it with a first-parent hop — generation 1, distance 1,
/// which `effective_distance()` charges 65536 for. 65535 < 65536, so the younger tag
/// takes it.
///
/// Preferring the older tagger date first kept `near-old~1`.
#[test]
fn the_nearer_tag_wins_even_when_it_is_the_younger_one() {
    let (f, target) = two_ways_to_reach_one_commit("nearer");

    let out = f.run(&["name-rev", "--tags", "--name-only", &target]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "far-young^2\n");
}

/// The same contest without `--tags`, where the branch tips are in the table too.
/// They are not `from_tag`, so `is_better_name()`'s *second* arm keeps them out of
/// it (builtin/name-rev.c:126-128) and the answer is unchanged but for git's
/// `tags/` prefix, which `--tags --name-only` shortens away
/// (`add_to_tip_table`'s `shorten_unambiguous`, `:328-341`).
#[test]
fn branch_tips_do_not_disturb_the_contest_between_two_tags() {
    let (f, target) = two_ways_to_reach_one_commit("with-branches");

    let out = f.run(&["name-rev", "--name-only", &target]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "tags/far-young^2\n");
}

/// The older tag does win when it is not farther: reached directly, it is at
/// distance 0 and the merge parent cannot beat it, so the tie-keeping rule and the
/// tip order hold the name.
#[test]
fn the_older_tag_keeps_a_commit_it_sits_on() {
    let (f, _) = two_ways_to_reach_one_commit("exact");
    let tagged = f.rev("near-old^{commit}");

    let out = f.run(&["name-rev", "--tags", "--name-only", &tagged]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "near-old\n");
}

/// `describe --contains` delegates to exactly this walk, so it answers the same
/// thing (builtin/describe.c:710-748).
#[test]
fn describe_contains_inherits_the_distance_rule() {
    let (f, target) = two_ways_to_reach_one_commit("contains");

    let describe = f.run(&["describe", "--contains", &target]);
    assert!(describe.status.success(), "{}", stderr(&describe));
    assert_eq!(stdout(&describe), "far-young^2\n");

    let name_rev =
        f.run(&["name-rev", "--peel-tag", "--name-only", "--no-undefined", "--tags", &target]);
    assert_eq!(stdout(&describe), stdout(&name_rev));
}
