//! `git describe`'s candidate walk reports the parents it cannot read, and keeps
//! walking.
//!
//! `describe_commit()` calls `repo_parse_commit(the_repository, p)` for every
//! parent of every commit it pops and ignores the return
//! (builtin/describe.c:279 and `:429`), so `parse_commit_gently()`'s
//! `error("Could not read %s")` is a pure stderr side effect: the traversal does
//! not abort, and the exit code is still decided by whether a candidate was found.
//! That is the opposite contract from the aborting lookups `log`, `rev-list` and
//! `merge-base` need, which is why the reporting lives in the describe verb rather
//! than in the shared traversal.
//!
//! A graft naming a parent the repository does not have is the everyday way to
//! reach it. Every expectation below was taken from a differential run against
//! stock git 2.55.0 in the same fixture; the tests shell out to nothing but the
//! binary under test.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// An object id no repository will have.
const MISSING: &str = "0000000000000000000000000000000000000001";

fn run(repo: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        // The graft file is deprecated and git says so on every read; silencing it
        // keeps these assertions about the walk rather than about the advice.
        .args(["-c", "advice.graftFileDeprecated=false"])
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
        .env("LC_ALL", "C")
        .output()
        .unwrap()
}

fn ok(repo: &Path, home: &Path, args: &[&str]) -> Output {
    let out = run(repo, home, args);
    assert!(
        out.status.success(),
        "setup `git {args:?}` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Three commits with an annotated tag on the first, and — once [`Fixture::graft`]
/// is called — a graft that replaces the middle commit's parents with an object
/// that is not there, cutting the tag off from `HEAD`.
struct Fixture {
    root: PathBuf,
    repo: PathBuf,
    home: PathBuf,
    tagged: String,
    middle: String,
    head: String,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-descgraft-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        let home = root.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        let mut f = Fixture {
            root,
            repo,
            home,
            tagged: String::new(),
            middle: String::new(),
            head: String::new(),
        };
        ok(&f.repo, &f.home, &["init", "-q", "-b", "main", "."]);
        let mut ids = Vec::new();
        for i in 0..3 {
            std::fs::write(f.repo.join(format!("f{i}")), format!("{i}\n")).unwrap();
            ok(&f.repo, &f.home, &["add", &format!("f{i}")]);
            ok(&f.repo, &f.home, &["commit", "-q", "-m", &format!("c{i}")]);
            if i == 0 {
                ok(&f.repo, &f.home, &["tag", "-a", "-m", "annotated", "v1.0"]);
            }
            ids.push(stdout(&ok(&f.repo, &f.home, &["rev-parse", "HEAD"])).trim().to_owned());
        }
        f.tagged = ids[0].clone();
        f.middle = ids[1].clone();
        f.head = ids[2].clone();
        f
    }

    /// Point the middle commit's parent list at [`MISSING`].
    fn graft(&self) {
        let info = self.repo.join(".git/info");
        std::fs::create_dir_all(&info).unwrap();
        std::fs::write(info.join("grafts"), format!("{} {MISSING}\n", self.middle)).unwrap();
    }

    fn describe(&self, args: &[&str]) -> Output {
        let mut argv = vec!["describe"];
        argv.extend_from_slice(args);
        run(&self.repo, &self.home, &argv)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// `v1.0-2-g<abbrev>`, without pinning the abbreviation length — it is sized from
/// the object count and would make this a test of `core.abbrev` instead.
fn assert_long_form(stdout: &str, head: &str) {
    let line = stdout.trim();
    let abbrev = line
        .strip_prefix("v1.0-2-g")
        .unwrap_or_else(|| panic!("expected `v1.0-2-g<abbrev>`, got {line:?}"));
    assert!(
        !abbrev.is_empty() && head.starts_with(abbrev),
        "{abbrev:?} is not an abbreviation of {head}"
    );
}

fn could_not_read_lines(stderr: &str) -> usize {
    stderr.lines().filter(|l| *l == format!("error: Could not read {MISSING}")).count()
}

/// The walk reaches the unreadable parent, says so, and still reaches git's own
/// verdict — the error is not a failure of its own, and it precedes the `fatal:`.
#[test]
fn an_unreadable_grafted_parent_is_reported_before_the_fatal() {
    let f = Fixture::new("fatal");
    f.graft();
    let out = f.describe(&[]);

    assert_eq!(
        stderr(&out),
        format!(
            "error: Could not read {MISSING}\n\
             fatal: No tags can describe '{}'.\n\
             Try --always, or create some tags.\n",
            f.head
        )
    );
    assert_eq!(out.status.code(), Some(128), "the fatal still decides the exit code");
    assert!(stdout(&out).is_empty());
}

/// The reporting is a side effect of the walk, not of failing: with `--always` the
/// command succeeds and still prints it.
#[test]
fn the_report_survives_a_successful_describe() {
    let f = Fixture::new("always");
    f.graft();
    let out = f.describe(&["--always"]);

    assert_eq!(stderr(&out), format!("error: Could not read {MISSING}\n"));
    assert!(out.status.success(), "--always must still succeed");
    assert!(
        f.head.starts_with(stdout(&out).trim()),
        "expected an abbreviation of {}, got {:?}",
        f.head,
        stdout(&out)
    );
}

/// The port answers "would unannotated tags have helped?" with a second walk that
/// git does not make (git counts it inside the one walk it already ran). That
/// walk must stay silent, or one `git describe` would report the same unreadable
/// parent twice.
#[test]
fn the_unreadable_parent_is_reported_exactly_once() {
    let f = Fixture::new("once");
    f.graft();
    let out = f.describe(&[]);
    assert_eq!(
        could_not_read_lines(&stderr(&out)),
        1,
        "expected exactly one report:\n{}",
        stderr(&out)
    );
}

/// `--match` resolves through a hand-built candidate map rather than gix's
/// selector platform. Both routes have to report.
#[test]
fn a_glob_filtered_walk_reports_too() {
    let f = Fixture::new("glob");
    f.graft();
    let out = f.describe(&["--match", "v*"]);
    assert_eq!(
        stderr(&out),
        format!(
            "error: Could not read {MISSING}\n\
             fatal: No tags can describe '{}'.\n\
             Try --always, or create some tags.\n",
            f.head
        )
    );
}

/// `--exact-match` is `max_candidates == 0`, which git turns into "no walk at
/// all" — so there is no parent lookup to report on.
#[test]
fn exact_match_reports_nothing_because_it_never_walks() {
    let f = Fixture::new("exact");
    f.graft();
    let out = f.describe(&["--exact-match"]);
    assert_eq!(stderr(&out), format!("fatal: no tag exactly matches '{}'\n", f.head));
    assert_eq!(out.status.code(), Some(128));
}

/// A walk that never needs the grafted commit's parents never reports: here the
/// tag is found before the traversal gets that far.
#[test]
fn a_walk_that_stops_short_reports_nothing() {
    let f = Fixture::new("short");
    // Graft the *tagged* commit, which the walk only reaches after it has already
    // named `HEAD` from the tag sitting on it.
    let info = f.repo.join(".git/info");
    std::fs::create_dir_all(&info).unwrap();
    std::fs::write(info.join("grafts"), format!("{} {MISSING}\n", f.tagged)).unwrap();

    let out = f.describe(&[]);
    assert!(out.status.success(), "describe should have succeeded: {}", stderr(&out));
    assert_eq!(could_not_read_lines(&stderr(&out)), 0, "nothing to report:\n{}", stderr(&out));
    assert_long_form(&stdout(&out), &f.head);
}

/// Nothing missing, nothing said. The regression guard for the reporting hook
/// being attached to *every* object lookup the graph makes.
#[test]
fn an_intact_repository_reports_nothing() {
    let f = Fixture::new("intact");
    let out = f.describe(&[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stderr(&out), "", "an intact walk must be silent");
    assert_long_form(&stdout(&out), &f.head);
}

/// A graft pointing at an object that *is* there but is not a commit is git's
/// other `parse_commit_gently()` complaint, with its own wording.
#[test]
fn a_grafted_parent_that_is_not_a_commit_is_reported_with_gits_other_wording() {
    let f = Fixture::new("blob");
    let blob = stdout(&ok(&f.repo, &f.home, &["hash-object", "-w", "f0"])).trim().to_owned();
    let info = f.repo.join(".git/info");
    std::fs::create_dir_all(&info).unwrap();
    std::fs::write(info.join("grafts"), format!("{} {blob}\n", f.middle)).unwrap();

    let out = f.describe(&[]);
    assert_eq!(
        stderr(&out),
        format!(
            "error: Object {blob} not a commit\n\
             fatal: No tags can describe '{}'.\n\
             Try --always, or create some tags.\n",
            f.head
        )
    );
    assert_eq!(out.status.code(), Some(128));
}
