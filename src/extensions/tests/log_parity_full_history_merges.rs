//! A TREESAME merge is kept when the caller wants ancestry.
//!
//! `get_commit_action()` does not simply drop every commit the pathspec found no
//! change in:
//!
//! ```c
//! if (commit->object.flags & TREESAME) {
//!         int n;
//!         struct commit_list *p;
//!         /* drop merges unless we want parenthood */
//!         if (!want_ancestry(revs))
//!                 return commit_ignore;
//!
//!         if (revs->show_pulls && (commit->object.flags & PULL_MERGE))
//!                 return commit_show;
//!
//!         /*
//!          * If we want ancestry, then need to keep any merges
//!          * between relevant commits to tie together topology.
//!          */
//!         for (n = 0, p = commit->parents; p; p = p->next)
//!                 if (relevant_commit(p->item))
//!                         if (++n >= 2)
//!                                 return commit_show;
//!         return commit_ignore;
//! }
//! ```
//! (`revision.c:4221-4245`, v2.55.0)
//!
//! `want_ancestry()` is `revs->rewrite_parents || revs->children.name`
//! (`revision.c:3914-3917`), which `--parents`, `--graph`, `--simplify-merges`
//! and `--children` turn on.
//!
//! The rule only has teeth under `--full-history`, because that is the one mode
//! where the `REV_TREE_SAME` arm of `try_to_simplify_commit()` records the
//! parent and continues instead of pruning the parent list down to one
//! (`revision.c:1038-1048`): a merge therefore arrives at the display filter
//! still carrying both sides. Dropping it split the history into two strands
//! that `--parents` then reported as unrelated.
//!
//! The fixture is the graph t6012 builds, reduced to what the rule needs:
//!
//! ```text
//! A---B---G---H
//!  \     /
//!   C---E
//! ```
//!
//! `file` changes in A, B and C; `E` (merge of C and B) and `H` (merge of G and
//! E) change nothing in it, and `G` changes only `elif`.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    work: PathBuf,
    /// Every commit gets its own timestamp: with a shared one the walk order of
    /// the two sides is a tie the priority queue breaks by insertion, which is
    /// not what this file is measuring.
    tick: i64,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-log-fullhist-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let mut f = Fixture { root, work, tick: 1_700_000_100 };
        f.git(&["init", "-q", "-b", "main", "."]);

        f.write("file", "Hi there\n");
        f.git(&["add", "file"]);
        f.commit("A");
        f.git(&["branch", "side"]);

        // B changes `file` on main.
        f.write("file", "Hello\n");
        f.git(&["add", "file"]);
        f.commit("B");

        // C makes the identical change on the side branch, so the merge below is
        // TREESAME to both of its parents over `file`.
        f.git(&["checkout", "-q", "side"]);
        f.write("file", "Hello\n");
        f.git(&["add", "file"]);
        f.commit("C");

        // E: merge of C and B. Both sides already agree on `file`.
        f.tick += 100;
        f.git(&["merge", "-q", "--no-ff", "-m", "E", "main"]);
        f.git(&["tag", "E"]);

        // G touches an unrelated path on main.
        f.git(&["checkout", "-q", "main"]);
        f.write("elif", "Yet another\n");
        f.git(&["add", "elif"]);
        f.commit("G");

        // H: merge of G and E, again TREESAME over `file`.
        f.tick += 100;
        f.git(&["merge", "-q", "--no-ff", "-m", "H", "side"]);
        f.git(&["tag", "H"]);
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

    fn subjects(&self, args: &[&str]) -> Vec<String> {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
        String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect()
    }
}

/// With `--parents` in play the two TREESAME merges stay, because each of them
/// joins two relevant commits.
#[test]
fn full_history_keeps_treesame_merges_between_relevant_parents() {
    let f = Fixture::new("parents");
    assert_eq!(
        f.subjects(&["log", "--format=%s", "--parents", "--full-history", "--", "file"]),
        vec!["H", "E", "C", "B", "A"]
    );
    // `--graph` and `--children` reach `want_ancestry()` by the other two routes.
    assert_eq!(
        f.subjects(&["log", "--format=%s", "--graph", "--full-history", "--", "file"])
            .iter()
            .map(|l| l.trim_start_matches(['*', '|', '\\', '/', ' ']).to_string())
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>(),
        vec!["H", "E", "C", "B", "A"]
    );
    assert_eq!(
        f.subjects(&["log", "--format=%s", "--children", "--full-history", "--", "file"]),
        vec!["H", "E", "C", "B", "A"]
    );
}

/// Without a `want_ancestry()` flag the same walk drops both merges — the
/// `if (!want_ancestry(revs)) return commit_ignore;` arm. This is the half of
/// the rule that already held, and it must keep holding.
#[test]
fn full_history_alone_still_drops_treesame_merges() {
    let f = Fixture::new("plain");
    assert_eq!(
        f.subjects(&["log", "--format=%s", "--full-history", "--", "file"]),
        vec!["C", "B", "A"]
    );
}

/// The count is over `commit->parents`, and an excluded side is still a BOTTOM,
/// which `relevant_commit()` treats as relevant — so `E ^C` keeps the merge and
/// walks only the other side. Measured from stock 2.55.0 on this fixture.
#[test]
fn an_excluded_side_is_a_bottom_and_stays_relevant() {
    let f = Fixture::new("excluded");
    let kept = f.subjects(&["log", "--format=%s", "--parents", "--full-history", "E", "--", "file"]);
    assert_eq!(kept, vec!["E", "C", "B", "A"]);

    let kept = f.subjects(&["log", "--format=%s", "--parents", "--full-history", "E", "^C", "--", "file"]);
    assert_eq!(kept, vec!["E", "B"]);
}

/// Ancestry the output prints is still the real one under `--full-history`:
/// `rewrite_parents()` runs, but `mark_redundant_parents()` belongs to
/// `--simplify-merges`, so the kept merge names both sides.
#[test]
fn a_kept_merge_prints_both_parents() {
    let f = Fixture::new("ancestry");
    let lines = f.subjects(&["log", "--format=%h %p %s", "--parents", "--full-history", "--", "file"]);
    let h = lines.first().expect("H is shown");
    assert!(h.ends_with(" H"), "{h}");
    // "<hash> <parent> <parent> H" — two parents.
    assert_eq!(h.split(' ').count(), 4, "{h}");
}
