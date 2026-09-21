//! Three `setup_revisions()` options the walkers refused outright.
//!
//! **`--no-graph`**
//!
//! ```c
//! } else if (!strcmp(arg, "--graph")) {
//!         graph_clear(revs->graph);
//!         revs->graph = graph_init(revs);
//! } else if (!strcmp(arg, "--no-graph")) {
//!         graph_clear(revs->graph);
//!         revs->graph = NULL;
//! }
//! ```
//!
//! The graph's implied `--topo-order` and parent rewrite are not set there but in
//! `revision_opts_finish()`:
//!
//! ```c
//! if (revs->graph) {
//!         revs->topo_order = 1;
//!         revs->rewrite_parents = 1;
//! }
//! ```
//! (`revision.c:2749-2752`, v2.55.0)
//!
//! which runs after the whole command line, so `--graph --no-graph` keeps
//! neither while `--topo-order --no-graph` and `--parents --no-graph` keep their
//! own.
//!
//! **`--end-of-options`**
//!
//! ```c
//! if (!strcmp(arg, "--end-of-options")) {
//!         seen_end_of_options = 1;
//!         continue;
//! }
//! ```
//! (`revision.c:3062-3065`) — read ahead of `handle_revision_opt()`, and its
//! effect is on the loop guard `if (!seen_end_of_options && *arg == '-')`
//! (`revision.c:3040`), so afterwards a token that looks like an option is an
//! operand. `--` is still a separator: it is found in a scan that runs before the
//! loop.
//!
//! **`--ignore-missing`**
//!
//! ```c
//! if (get_oid_with_context(revs->repo, arg, get_sha1_flags, &oid, &oc)) {
//!         ret = revs->ignore_missing ? 0 : -1;
//!         goto out;
//! }
//! ```
//! (`revision.c:2223-2226`) — a `0` is `handle_revision_arg()` succeeding with
//! nothing pended, so it sets `revs->rev_input_given` and the walk does not fall
//! back to `HEAD`, and it stands ahead of the filename fallback.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");
const ZERO: &str = "0000000000000000000000000000000000000000";

struct Fixture {
    root: PathBuf,
    work: PathBuf,
    tick: i64,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// A three-commit trunk with a one-commit side branch merged in, plus a
    /// branch literally called `--source`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-log-revflags-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let mut f = Fixture { root, work, tick: 1_700_000_100 };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.commit_file("a", "one");
        f.git(&["checkout", "-q", "-b", "side"]);
        f.commit_file("b", "side");
        f.git(&["checkout", "-q", "main"]);
        f.commit_file("c", "two");
        f.tick += 100;
        f.git(&["merge", "-q", "--no-ff", "-m", "merge", "side"]);
        f.git(&["update-ref", "refs/heads/--source", "HEAD"]);
        f
    }

    fn commit_file(&mut self, name: &str, msg: &str) {
        std::fs::write(self.work.join(name), format!("{msg}\n")).unwrap();
        self.git(&["add", name]);
        self.tick += 100;
        self.git(&["commit", "-q", "-m", msg]);
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", format!("{} +0000", self.tick))
            .env("GIT_COMMITTER_DATE", format!("{} +0000", self.tick))
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = self.cmd(args).output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn stdout(&self, args: &[&str]) -> String {
        let (out, err, code) = self.run(args);
        assert_eq!((err.as_str(), code), ("", 0), "`git {args:?}`");
        out
    }
}

/// `--no-graph` undoes `--graph` — the drawing *and* the order and parent
/// rewrite it would have implied — and leaves an explicitly requested order or
/// `--parents` alone.
#[test]
fn no_graph_countermands_graph_without_touching_the_rest() {
    let f = Fixture::new("nograph");

    for verb in ["log", "rev-list"] {
        let plain = f.stdout(&[verb, "--format=%H", "--all"]);
        assert_eq!(f.stdout(&[verb, "--format=%H", "--graph", "--no-graph", "--all"]), plain, "{verb}");
        // The last spelling wins in the other direction too.
        assert_ne!(f.stdout(&[verb, "--format=%H", "--no-graph", "--graph", "--all"]), plain, "{verb}");

        let topo = f.stdout(&[verb, "--format=%H", "--topo-order", "--all"]);
        assert_eq!(
            f.stdout(&[verb, "--format=%H", "--topo-order", "--no-graph", "--all"]),
            topo,
            "{verb}"
        );

        let parents = f.stdout(&[verb, "--format=%H", "--parents", "--all"]);
        assert_eq!(
            f.stdout(&[verb, "--format=%H", "--parents", "--no-graph", "--all"]),
            parents,
            "{verb}"
        );
    }
}

/// After `--end-of-options` a token that looks like an option is a revision, so
/// the branch named `--source` is walked rather than `--source` being turned on.
#[test]
fn end_of_options_turns_later_options_into_operands() {
    let f = Fixture::new("eoo");

    let head = f.stdout(&["rev-list", "HEAD"]);
    assert_eq!(f.stdout(&["rev-list", "--end-of-options", "--source"]), head);
    assert_eq!(f.stdout(&["log", "--format=%H", "--end-of-options", "--source"]), head);

    // Options *before* it are still options.
    assert_eq!(f.stdout(&["rev-list", "--max-count=1", "--end-of-options", "--source"]).lines().count(), 1);

    // A value-taking long option no longer claims its argument: `--grep` is the
    // branch, and `merge` is a second operand — not a pattern.
    let (_, err, code) = f.run(&["rev-list", "--end-of-options", "--grep", "merge"]);
    assert_eq!(code, 128, "{err}");
    assert!(err.contains("--grep"), "{err}");

    // `--` is found in a scan that runs before the option loop, so it separates
    // regardless of where `--end-of-options` stands.
    assert_eq!(f.stdout(&["rev-list", "--end-of-options", "HEAD", "--", "a"]).lines().count(), 1);
}

/// An object name the database does not have is silently dropped, and the walk
/// does not fall back to `HEAD` — the operand still counted as revision input.
#[test]
fn ignore_missing_drops_the_operand_without_defaulting_to_head() {
    let f = Fixture::new("ignoremissing");

    for verb in ["log", "rev-list"] {
        let (out, err, code) = f.run(&[verb, "--format=%H", "--ignore-missing", ZERO]);
        assert_eq!((out.as_str(), err.as_str(), code), ("", "", 0), "{verb}");
    }

    // Without it the same operand is fatal.
    let (_, _, code) = f.run(&["rev-list", ZERO]);
    assert_eq!(code, 128);

    // A real revision beside it is still walked.
    let head = f.stdout(&["rev-list", "HEAD"]);
    assert_eq!(f.stdout(&["rev-list", "--ignore-missing", ZERO, "HEAD"]), head);
}
