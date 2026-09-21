//! `--cherry-pick` / `--cherry-mark` compare the change *within the pathspec*.
//!
//! ```c
//! left_first = left_count < right_count;
//! init_patch_ids(revs->repo, &ids);
//! ids.diffopts.pathspec = revs->diffopt.pathspec;
//! ```
//! (`revision.c:1240-1242`, v2.55.0)
//!
//! `cherry_pick_list()` hands the walk's own pathspec to the patch-id machinery
//! before it hashes a single diff, so `A...B -- <path>` asks whether two commits
//! made the same change to that path — not whether they are the same commit.
//! The port hashed the whole diff, so a pair that agreed on the limited path and
//! differed anywhere else came out distinct: `--cherry-pick` dropped nothing and
//! `--cherry-mark` printed `+` where stock prints `=`.
//!
//! The fixture is t6007's, cut down: two branches whose commits touch `bar`
//! identically while their other paths differ.
//!
//! ```text
//!        B---C---E   (right)
//!       /
//!   A--+
//!       \
//!        D---F       (left)
//! ```
//!
//! `C` and `D` write the same content to `bar`; everything else about them
//! differs. `E` and `F` are the tips and touch `bar` differently.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-log-cherrypath-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let mut f = Fixture { root, work, tick: 1_700_000_100 };
        f.git(&["init", "-q", "-b", "main", "."]);

        f.write("foo", "base\n");
        f.write("bar", "base\n");
        f.git(&["add", "foo", "bar"]);
        f.commit("A");

        // Right side: B touches only `foo`, C makes the shared `bar` change.
        f.write("foo", "right\n");
        f.git(&["add", "foo"]);
        f.commit("B");
        f.write("bar", "shared\n");
        f.write("foo", "right-2\n");
        f.git(&["add", "bar", "foo"]);
        f.commit("C");
        f.write("bar", "right-tip\n");
        f.git(&["add", "bar"]);
        f.commit("E");

        // Left side off A: D makes the same `bar` change with a different `foo`.
        f.git(&["checkout", "-q", "-b", "left", "A"]);
        f.write("bar", "shared\n");
        f.write("foo", "left-1\n");
        f.git(&["add", "bar", "foo"]);
        f.commit("D");
        f.write("bar", "left-tip\n");
        f.git(&["add", "bar"]);
        f.commit("F");
        f
    }

    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.work.join(name), body).unwrap();
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

    fn commit(&mut self, msg: &str) {
        self.tick += 100;
        self.git(&["commit", "-q", "-m", msg]);
        self.git(&["tag", msg]);
    }

    fn lines(&self, args: &[&str]) -> Vec<String> {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
        String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect()
    }

    /// `rev-list` output with each object name replaced by the tag on it, so the
    /// expectations read as the graph above.
    fn named(&self, args: &[&str]) -> Vec<String> {
        let mut named: Vec<(String, &str)> = Vec::new();
        for tag in ["A", "B", "C", "D", "E", "F"] {
            named.push((self.lines(&["rev-parse", tag]).remove(0), tag));
        }
        self.lines(args)
            .into_iter()
            .map(|mut line| {
                for (oid, tag) in &named {
                    line = line.replace(oid.as_str(), tag);
                }
                line
            })
            .collect()
    }
}

/// `C` and `D` are the same patch over `bar`, so `--cherry-mark` marks both with
/// `=` and `--cherry-pick` drops both.
#[test]
fn a_pathspec_narrows_what_counts_as_the_same_patch() {
    let f = Fixture::new("mark");

    assert_eq!(
        f.named(&["rev-list", "--cherry-mark", "--left-right", "F...E", "--", "bar"]),
        vec!["<F", "=D", ">E", "=C"]
    );
    assert_eq!(
        f.named(&["rev-list", "--cherry-pick", "--left-right", "F...E", "--", "bar"]),
        vec!["<F", ">E"]
    );
    assert_eq!(f.lines(&["rev-list", "--cherry", "--count", "F...E", "--", "bar"]), vec!["1\t1"]);
    assert_eq!(
        f.lines(&["rev-list", "--cherry-mark", "--left-right", "--count", "F...E", "--", "bar"]),
        vec!["1\t1\t2"]
    );
}

/// Without the pathspec the two commits are different patches — `B`'s and `D`'s
/// other paths diverge — so nothing is equivalent and every commit is listed.
#[test]
fn without_a_pathspec_the_whole_diff_still_decides() {
    let f = Fixture::new("whole");
    assert_eq!(
        f.named(&["rev-list", "--cherry-mark", "--left-right", "F...E"]),
        vec!["<F", "<D", ">E", ">C", ">B"]
    );
    assert_eq!(
        f.named(&["rev-list", "--cherry-pick", "--left-right", "F...E"]),
        vec!["<F", "<D", ">E", ">C", ">B"]
    );
}

/// `git log` runs the same `cherry_pick_list()`, so it narrows identically.
#[test]
fn log_reads_the_same_limited_patch_ids() {
    let f = Fixture::new("log");
    assert_eq!(
        f.lines(&["log", "--format=%s", "--cherry-pick", "--left-right", "F...E", "--", "bar"]),
        vec!["F", "E"]
    );
    assert_eq!(
        f.lines(&["log", "--format=%s", "--cherry-pick", "--left-right", "F...E"]),
        vec!["F", "D", "E", "C", "B"]
    );
}

/// `--left-only`/`--right-only` read the same marks, which is the shape
/// `git cherry`-style callers use.
#[test]
fn one_sided_output_drops_the_equivalent_commit_too() {
    let f = Fixture::new("sided");
    assert_eq!(
        f.lines(&["log", "--format=%s", "--cherry-pick", "--right-only", "F...E", "--", "bar"]),
        vec!["E"]
    );
    assert_eq!(
        f.lines(&["log", "--format=%s", "--cherry-pick", "--left-only", "E...F", "--", "bar"]),
        vec!["E"]
    );
}
