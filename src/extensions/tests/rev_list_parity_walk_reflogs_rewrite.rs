//! `rev-list -g` parent lists: the rewrite through history outside the reflog,
//! and the parents `finish_commit()` frees.
//!
//! A reflog walk has no limited list. `get_revision_1()` runs
//! `try_to_simplify_commit()` on each entry as `next_reflog_entry()` hands it
//! out (revision.c:4386-4420), and for a shown entry under `--parents`,
//! `rewrite_parents()` → `rewrite_one_1()` runs `process_parents()` on every
//! commit it passes (revision.c:4035-4054) — the reflog does not have to name
//! them, and each is relevant unless UNINTERESTING (revision.c:524-527). zvcs
//! classified the entries against one another, so a parent outside the reflog
//! counted as irrelevant: `--sparse` kept both parents of a merge TREESAME to its
//! side, and nothing was rewritten past a commit the reflog skipped.
//!
//! `finish_commit()` frees a printed commit's parents
//! (builtin/rev-list.c:228-234), and `-g` can print one commit twice: the second
//! time it has none, and under a pathspec it is judged as a root. And a merge's
//! second parent is never parsed under `--first-parent` (neither
//! `try_to_simplify_commit()` nor `process_parents()` reaches it), so it is not
//! simplified and stays in the rewritten list as it is.
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
    /// ```text
    /// A(x) - B(y) - C(z) - M - D(y) - E(z)      main
    ///           \         /
    ///            S1(x) - S2(y)                  side
    /// ```
    /// `topic`'s reflog names M, then E, then M again; nothing between them.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-rl-g-rewrite-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(0, &["init", "-q", "-b", "main", "."]);
        f.commit(1, "x", "v1", "A");
        f.commit(2, "y", "v1", "B");
        f.run(3, &["checkout", "-q", "-b", "side"]);
        f.commit(3, "x", "v2", "S1");
        f.commit(4, "y", "v2", "S2");
        f.run(5, &["checkout", "-q", "main"]);
        f.commit(5, "z", "v3", "C");
        f.run(6, &["merge", "-q", "--no-ff", "side", "-m", "M"]);
        f.commit(7, "y", "v4", "D");
        f.commit(8, "z", "v5", "E");
        f.run(9, &["branch", "topic", "main~2"]);
        f.run(10, &["branch", "-f", "topic", "main"]);
        f.run(11, &["branch", "-f", "topic", "main~2"]);
        f
    }

    fn commit(&self, minute: u64, file: &str, body: &str, msg: &str) {
        std::fs::write(self.work.join(file), format!("{body}\n")).unwrap();
        self.run(minute, &["add", file]);
        self.run(minute, &["commit", "-q", "-m", msg]);
    }

    /// Full object name of each commit, by subject.
    fn ids(&self) -> std::collections::HashMap<String, String> {
        let (out, _, _) = self.run(20, &["log", "--all", "--format=%s %H"]);
        out.lines()
            .map(|l| {
                let (s, h) = l.split_once(' ').unwrap();
                (s.to_string(), h.to_string())
            })
            .collect()
    }

    /// `names` with each subject replaced by its object name.
    fn expand(&self, names: &str) -> String {
        let ids = self.ids();
        names
            .lines()
            .map(|l| {
                let words: Vec<&str> =
                    l.split(' ').map(|w| ids.get(w).map_or(w, String::as_str)).collect();
                format!("{}\n", words.join(" "))
            })
            .collect()
    }

    fn run(&self, minute: u64, args: &[&str]) -> (String, String, i32) {
        let date = format!("{} +0000", 1_700_000_000 + 60 * minute);
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
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .env("GIT_MERGE_AUTOEDIT", "no")
            .env("GIT_PAGER", "cat")
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

    /// Runs `args` and returns stdout, asserting a clean exit.
    fn out(&self, args: &[&str]) -> String {
        let (out, err, code) = self.run(20, args);
        assert_eq!((err.as_str(), code), ("", 0), "{args:?}");
        out
    }
}

#[test]
fn rewrite_follows_history_the_reflog_skips() {
    let f = Fixture::new("dense");
    assert_eq!(f.out(&["rev-list", "-g", "--parents", "topic", "--", "z"]), f.expand("E C"));
    assert_eq!(
        f.out(&["rev-list", "-g", "--full-history", "--parents", "topic", "--", "z"]),
        f.expand("M C\nE M\nM")
    );
    assert_eq!(
        f.out(&["rev-list", "-g", "--full-history", "--parents", "topic", "--", "x"]),
        f.expand("M A S1\nM")
    );
}

#[test]
fn sparse_prunes_a_merge_to_the_side_outside_the_reflog() {
    let f = Fixture::new("sparse");
    assert_eq!(
        f.out(&["rev-list", "-g", "--sparse", "--parents", "topic", "--", "x"]),
        f.expand("M S2\nE D\nM")
    );
}

#[test]
fn a_repeated_entry_prints_no_parents() {
    let f = Fixture::new("repeat");
    assert_eq!(f.out(&["rev-list", "-g", "--parents", "topic"]), f.expand("M C S2\nE D\nM"));
    let ids = f.ids();
    let (m, e) = (&ids["M"], &ids["E"]);
    let (c, s2, d) = (&ids["C"][..7], &ids["S2"][..7], &ids["D"][..7]);
    assert_eq!(
        f.out(&["rev-list", "-g", "--format=%p", "topic"]),
        format!("commit {m}\n{c} {s2}\ncommit {e}\n{d}\ncommit {m}\n")
    );
    // The freed list is what the parent-count filters see as well.
    assert_eq!(f.out(&["rev-list", "-g", "--min-parents=1", "topic"]), f.expand("M\nE"));
}

#[test]
fn first_parent_leaves_the_unparsed_side_alone() {
    let f = Fixture::new("first");
    assert_eq!(
        f.out(&["rev-list", "-g", "--first-parent", "--parents", "topic", "--", "x"]),
        f.expand("M A S2\nM")
    );
}
