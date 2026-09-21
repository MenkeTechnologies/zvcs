//! `-R` and the status letter, in the two places git means different things by it.
//!
//! For a command that builds its own queue, `-R` is
//! `options->flags.reverse_diff`, and `diff_addremove()` applies it while the
//! record is created:
//!
//! ```c
//! if (options->flags.reverse_diff)
//!         addremove = (addremove == '+' ? '-' :
//!                      addremove == '-' ? '+' : addremove);
//! ```
//!
//! `diff_resolve_rename_copy()` then reads the swapped filespecs, so a reversed
//! creation really is a deletion and every format that consults the status says
//! so — `--compact-summary` annotates it `(gone)`, not `(new)`.
//!
//! `builtin/diff-pairs.c` has no such stage. It is handed finished records and
//! swaps only what it prints, so stock `git diff-pairs -R --raw` still reports
//! `A` for a record it reversed, and its `--compact-summary` still says `(new)`.
//!
//! The port routed `diff-tree`'s patch and stat formats through its `diff-pairs`
//! implementation and inherited the second rule for both, so
//! `git diff-tree -R --stat --compact-summary` annotated a reversed creation
//! `(new)` — and, since the annotation is part of the name, laid out the whole
//! column one character narrow.
//!
//! Every expectation was measured from stock git 2.55.0 over the same fixture.
#![cfg(unix)]

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
    /// Two commits: the second modifies `keep` and creates `born`, so one pair
    /// reverses into itself and one reverses into a deletion.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-diff-rev-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("keep"), "a\n").unwrap();
        f.git(&["add", "keep"]);
        f.git(&["commit", "-q", "-m", "one"]);
        std::fs::write(f.work.join("keep"), "b\n").unwrap();
        std::fs::write(f.work.join("born"), "c\nd\n").unwrap();
        f.git(&["add", "keep", "born"]);
        f.git(&["commit", "-q", "-m", "two"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("ZVCS_HOME", self.root.join("zvcs"))
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
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn stdout(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().unwrap();
        assert_eq!(out.status.code(), Some(0), "`git {args:?}`: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn stdout_stdin(&self, args: &[&str], input: Vec<u8>) -> String {
        use std::io::Write;
        let mut child = self
            .cmd(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(&input).unwrap();
        let out = child.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(0), "`git {args:?}`: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

/// A reversed creation is a deletion everywhere `diff-tree` reports a status.
#[test]
fn diff_tree_reverse_turns_a_creation_into_a_deletion() {
    let f = Fixture::new("tree");
    assert_eq!(
        f.stdout(&["diff-tree", "-r", "--name-status", "HEAD~1", "HEAD"]),
        "A\tborn\nM\tkeep\n"
    );
    assert_eq!(
        f.stdout(&["diff-tree", "-R", "-r", "--name-status", "HEAD~1", "HEAD"]),
        "D\tborn\nM\tkeep\n"
    );
    // `--stat --compact-summary` is the routed format that used to disagree: the
    // annotation and, through it, the width of the name column.
    assert_eq!(
        f.stdout(&["diff-tree", "-R", "-r", "--stat", "--compact-summary", "HEAD~1", "HEAD"]),
        " born (gone) | 2 --\n keep        | 2 +-\n 2 files changed, 1 insertion(+), 3 deletions(-)\n"
    );
    assert_eq!(
        f.stdout(&["diff-tree", "-r", "--stat", "--compact-summary", "HEAD~1", "HEAD"]),
        " born (new) | 2 ++\n keep       | 2 +-\n 2 files changed, 3 insertions(+), 1 deletion(-)\n"
    );
}

/// `git diff` builds its own queue too, so it answers the same way.
#[test]
fn diff_reverse_turns_a_creation_into_a_deletion() {
    let f = Fixture::new("diff");
    assert_eq!(
        f.stdout(&["diff", "-R", "--stat", "--compact-summary", "HEAD~1", "HEAD"]),
        " born (gone) | 2 --\n keep        | 2 +-\n 2 files changed, 1 insertion(+), 3 deletions(-)\n"
    );
}

/// `diff-pairs` is the exception: it swaps what it prints and nothing else, so a
/// reversed creation keeps its `A` and its `(new)`.
#[test]
fn diff_pairs_reverse_leaves_the_status_letter_alone() {
    let f = Fixture::new("pairs");
    let stream = f.stdout(&["diff-tree", "-z", "-r", "--raw", "HEAD~1", "HEAD"]).into_bytes();

    let raw = f.stdout_stdin(&["diff-pairs", "-R", "-z", "--raw"], stream.clone());
    let statuses: Vec<char> = raw
        .split('\0')
        .filter(|f| f.starts_with(':'))
        .filter_map(|f| f.split_whitespace().last().and_then(|s| s.chars().next()))
        .collect();
    assert_eq!(statuses, ['A', 'M'], "{raw:?}");

    let stat = f.stdout_stdin(&["diff-pairs", "-R", "-z", "--stat", "--compact-summary"], stream);
    assert_eq!(
        stat,
        " born (new) | 2 --\n keep       | 2 +-\n 2 files changed, 1 insertion(+), 3 deletions(-)\n"
    );
}
