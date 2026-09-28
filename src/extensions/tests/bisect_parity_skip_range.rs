//! `git bisect skip <a>..<b>` and the flags its walk leaves behind.
//!
//! `bisect_skip()` (builtin/bisect.c:1111-1146) expands the range with
//! `setup_revisions()` / `get_revision()` and then runs `bisect_state()` — and the
//! bisection step — in the same process. `reset_revision_walk()`
//! (revision.c:3678-3682) clears only `SEEN | ADDED | SHOWN`, so the
//! `UNINTERESTING` the excluded end and the ancestry `limit_list()` reached picked
//! up survives into `check_ancestors()` (bisect.c:890-908), whose
//! `clear_commit_marks_many()` only follows parsed commits, and into the
//! bisection walk (bisect.c:1075-1082). On a history whose commits share one
//! timestamp that decides the step: `skip c6..c9` leaves 3 revisions and tests
//! `c11`, where `skip c7 c8 c9` leaves 6 and tests `c12`. zvcs refused the range
//! form outright.
//!
//! A name that does not resolve is `verify_filename()`'s `ambiguous argument`
//! (setup.c), exit 128, before anything is written.
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
    /// `c1`..`c15` on one branch, each tagged, all with one committer date.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-bisect-skip-range-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "."]);
        for i in 1..=15 {
            std::fs::write(f.work.join("f"), format!("{i}\n")).unwrap();
            f.run(&["add", "f"]);
            f.run(&["commit", "-q", "-m", &format!("c{i}")]);
            f.run(&["tag", &format!("c{i}")]);
        }
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
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

    fn id(&self, name: &str) -> String {
        self.run(&["rev-parse", name]).0.trim().to_owned()
    }

    /// The `git bisect skip <oid>` lines of the log, by tag name.
    fn skipped(&self) -> Vec<String> {
        let names: Vec<(String, String)> = (1..=15).map(|i| (self.id(&format!("c{i}")), format!("c{i}"))).collect();
        self.run(&["bisect", "log"])
            .0
            .lines()
            .filter_map(|l| l.strip_prefix("git bisect skip "))
            .map(|oid| names.iter().find(|(id, _)| id == oid).map_or(oid.to_owned(), |(_, n)| n.clone()))
            .collect()
    }

    fn step(&self, args: &[&str]) -> (String, i32) {
        let (out, _, code) = self.run(args);
        (out, code)
    }
}

fn bisecting(left: &str, commit: &str, f: &Fixture) -> String {
    format!("Bisecting: {left}\n[{}] {commit}\n", f.id(commit))
}

#[test]
fn a_range_shrinks_the_step_the_same_revisions_do_not() {
    let f = Fixture::new("shrink");
    f.run(&["bisect", "start", "c15", "c1"]);
    assert_eq!(
        f.step(&["bisect", "skip", "c6..c9"]),
        (bisecting("3 revisions left to test after this (roughly 2 steps)", "c11", &f), 0)
    );
    // `get_revision()` order: newest first.
    assert_eq!(f.skipped(), ["c9", "c8", "c7"]);
    f.run(&["bisect", "reset"]);

    f.run(&["bisect", "start", "c15", "c1"]);
    assert_eq!(
        f.step(&["bisect", "skip", "c7", "c8", "c9"]),
        (bisecting("6 revisions left to test after this (roughly 3 steps)", "c12", &f), 0)
    );
}

#[test]
fn successive_ranges_and_empty_sides() {
    let f = Fixture::new("sides");
    f.run(&["bisect", "start", "c15", "c1"]);
    let expect = [
        ("c5..c3", "4 revisions left to test after this (roughly 2 steps)", "c10"),
        ("..c3", "2 revisions left to test after this (roughly 1 step)", "c12"),
        ("c13..", "0 revisions left to test after this (roughly 0 steps)", "c14"),
        ("c9~2..c9", "3 revisions left to test after this (roughly 2 steps)", "c11"),
    ];
    for (range, left, commit) in expect {
        assert_eq!(f.step(&["bisect", "skip", range]), (bisecting(left, commit, &f), 0), "{range}");
    }
    // `c5..c3`, `..c3` (`HEAD..c3`) and `c13..` (`c13..HEAD`) are all empty, which
    // leaves `bisect_state()` no operand, so each skips the checked-out `HEAD`.
    assert_eq!(f.skipped(), ["c8", "c10", "c12", "c9", "c8"]);
}

#[test]
fn a_range_that_does_not_resolve_writes_nothing() {
    let f = Fixture::new("bad");
    f.run(&["bisect", "start", "c15", "c1"]);
    let log = f.run(&["bisect", "log"]).0;
    assert_eq!(
        f.run(&["bisect", "skip", "nosuch..c5"]),
        (
            String::new(),
            "fatal: ambiguous argument 'nosuch..c5': unknown revision or path not in the working tree.\n\
             Use '--' to separate paths from revisions, like this:\n\
             'git <command> [<revision>...] -- [<file>...]'\n"
                .to_owned(),
            128
        )
    );
    assert_eq!(f.run(&["bisect", "log"]).0, log);
}
