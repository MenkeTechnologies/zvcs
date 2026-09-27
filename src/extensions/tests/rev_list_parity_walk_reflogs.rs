//! `git rev-list -g` next to the `setup_revisions()` rules that govern it.
//!
//! - `if (revs->reflog_info && revs->limited) die("cannot combine
//!   --walk-reflogs with history-limiting options")` (revision.c:3180-3181):
//!   `--topo-order`, `--graph`, `--cherry-pick`, `--children`, ... all set
//!   `revs->limited`. zvcs walked the reflog and ignored them.
//! - `die_for_incompatible_opt3(graph, reverse, reflog_info)`
//!   (revision.c:3190-3192): `--reverse` with `-g` is refused. zvcs reversed.
//! - `add_pending_object_with_path()` (revision.c:305-318) hands a commit
//!   named after `-g` to `add_reflog_for_walk()`, which dies with
//!   `cannot walk reflogs for <name>` on an UNINTERESTING one
//!   (reflog-walk.c:165-166). zvcs walked the excluded ref's log.
//! - The ref-set options (`--branches`, `--all`, ...) pend each ref under its
//!   name, so each names a reflog to walk. zvcs only walked operands typed
//!   out, and fell back to `HEAD` for everything else.
//! - `rev-list` has no default revision, so a `-g` that names no reflog walks
//!   nothing; an operand read *before* `-g` is an ordinary pending commit,
//!   and when UNINTERESTING it hides what it reaches from the reflog entries.
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
    /// `main`: A-B-C; `side` branches at B and adds S; `lw` is a lightweight
    /// tag on A (tags keep no reflog).
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-rev-list-walk-reflogs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        for n in ["A", "B", "C"] {
            std::fs::write(f.work.join("f"), format!("{n}\n")).unwrap();
            f.run(&["add", "f"]);
            f.run(&["commit", "-q", "-m", n]);
        }
        f.run(&["tag", "lw", "HEAD~2"]);
        f.run(&["branch", "side", "HEAD~1"]);
        f.run(&["checkout", "-q", "side"]);
        std::fs::write(f.work.join("s"), "s\n").unwrap();
        f.run(&["add", "s"]);
        f.run(&["commit", "-q", "-m", "S"]);
        f.run(&["checkout", "-q", "main"]);
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

    fn rev(&self, spec: &str) -> String {
        self.run(&["rev-parse", spec]).0
    }
}

#[test]
fn limiting_options_and_reverse_are_refused() {
    let f = Fixture::new("refused");
    for opt in ["--topo-order", "--date-order", "--graph", "--cherry-pick", "--children", "--simplify-merges"] {
        let (out, err, code) = f.run(&["rev-list", "-g", opt, "main"]);
        assert_eq!(
            (out.as_str(), err.as_str(), code),
            ("", "fatal: cannot combine --walk-reflogs with history-limiting options\n", 128),
            "{opt}"
        );
    }
    let (out, err, code) = f.run(&["rev-list", "-g", "--reverse", "main"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "fatal: options '--reverse' and '--walk-reflogs' cannot be used together\n", 128)
    );
}

#[test]
fn an_excluded_reflog_is_refused() {
    let f = Fixture::new("excluded");
    for args in [&["rev-list", "-g", "main..side"][..], &["rev-list", "-g", "--not", "--branches"]] {
        let (out, err, code) = f.run(args);
        assert_eq!(
            (out.as_str(), err.as_str(), code),
            ("", "fatal: cannot walk reflogs for main\n", 128),
            "{args:?}"
        );
    }
}

#[test]
fn ref_sets_name_reflogs_and_nothing_defaults_to_head() {
    let f = Fixture::new("sets");
    let both = f.run(&["rev-list", "-g", "main", "side"]);
    assert_eq!(both.2, 0);
    assert_eq!(both.0.lines().count(), 5);
    assert_eq!(f.run(&["rev-list", "-g", "--branches"]), both);
    // A lightweight tag has no reflog, and `rev-list` has no `HEAD` default.
    assert_eq!(f.run(&["rev-list", "-g", "--tags"]), (String::new(), String::new(), 0));
    // Pended before `-g`: an ordinary commit, not a reflog to walk.
    assert_eq!(f.run(&["rev-list", "main", "-g"]), (String::new(), String::new(), 0));
    // Pended UNINTERESTING before `-g`: not refused, but it hides B and A.
    assert_eq!(f.run(&["rev-list", "^side", "-g", "main"]), (f.rev("main"), String::new(), 0));
}
