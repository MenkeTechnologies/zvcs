//! The `--shortstat`/`--summary` block a sequencer verb prints after it commits,
//! and the one `git bisect` prints on its verdict — both pinned against stock
//! git 2.55.0.
//!
//! `print_commit_summary()` (sequencer.c:1413-1495) is the *same* function
//! `git commit` prints its own summary with, and it counts nothing itself: it
//! hands the commit to the ordinary revision/diff machinery with
//! `DIFF_FORMAT_SHORTSTAT | DIFF_FORMAT_SUMMARY` and
//! `rev.diffopt.detect_rename = DIFF_DETECT_RENAME`, then lets
//! `log_tree_commit()` diff it against its first parent. A hand-rolled tree walk
//! beside it drifts in two ways that both look like plausible output:
//!
//!   * rename detection is off, so a replayed `git mv` is reported as a create
//!     plus a delete carrying the moved file's whole line count;
//!   * the line counts come from `gix`'s tree-diff statistics, which run both
//!     blobs through the `Mode::ToGit` conversion pipeline *before* diffing them
//!     — so a commit whose only change is CRLF becoming LF diffs against itself
//!     and scores `0 insertions(+), 0 deletions(-)`.
//!
//! `git bisect`'s verdict has a third failure mode from the same family:
//! `gix_diff::tree_with_rewrites` reports a changed directory *as well as* the
//! files inside it, while git's recursive walk emits blob-level filepairs alone.
//! Left in, the containing trees are read as blobs and rendered as binary
//! (`sub | Bin 0 -> 37 bytes`, the raw tree object's size) and counted in the
//! `N files changed` total.

use std::path::PathBuf;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Repo {
    root: PathBuf,
    dir: PathBuf,
    home: PathBuf,
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Repo {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-seqstat-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("repo");
        let home = root.join("home");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let r = Repo { root, dir, home };
        r.git(&["init", "-q", "-b", "main"]);
        r
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.dir)
            .env("HOME", &self.home)
            .env("ZVCS_HOME", &self.home)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_EDITOR", "true")
            .env("GIT_SEQUENCE_EDITOR", "true")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@e.x")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@e.x")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000");
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().expect("run binary")
    }

    fn git(&self, args: &[&str]) {
        let o = self.run(args);
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    }

    /// Stdout of a command that must succeed.
    fn ok_stdout(&self, args: &[&str]) -> String {
        let o = self.run(args);
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).into_owned()
    }

    fn write(&self, rel: &str, body: &str) {
        let path = self.dir.join(rel);
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }
}

/// `seed` carries a CRLF file and `orig/m.txt`; `main~1` rewrites the CRLF as LF
/// and `main` moves `orig/m.txt` to `moved/m.txt` unchanged. `side` sits at
/// `seed`, so either commit can be replayed onto it.
fn crlf_and_rename(tag: &str) -> Repo {
    let r = Repo::new(tag);
    r.write("eol.txt", "a\r\nb\r\nc\r\n");
    r.write("orig/m.txt", "x\ny\nz\n");
    r.git(&["add", "-A"]);
    r.git(&["commit", "-q", "-m", "seed"]);
    r.write("eol.txt", "a\nb\nc\n");
    r.git(&["commit", "-qam", "crlf to lf"]);
    std::fs::create_dir_all(r.dir.join("moved")).unwrap();
    r.git(&["mv", "orig/m.txt", "moved/m.txt"]);
    r.git(&["commit", "-q", "-m", "pure rename"]);
    r.git(&["checkout", "-q", "-b", "side", "main~2"]);
    r
}

/// The lines after the `[<branch> <oid>] <subject>` headline, which carries an
/// abbreviated object id and so cannot be compared byte for byte.
fn body(text: &str) -> Vec<&str> {
    text.lines().skip(1).collect()
}

/// A cherry-pick whose only change is line endings. The stat has to see the raw
/// blobs: normalized on both sides the two are identical and the summary
/// collapses to `0 insertions(+), 0 deletions(-)` while still claiming
/// `1 file changed` — a self-contradictory line that reads as plausible output.
#[test]
fn cherry_pick_summary_counts_a_crlf_only_change() {
    let r = crlf_and_rename("cp-crlf");
    let out = r.ok_stdout(&["cherry-pick", "main~1"]);
    assert_eq!(
        body(&out),
        [
            " Date: Tue Nov 14 22:13:20 2023 +0000",
            " 1 file changed, 3 insertions(+), 3 deletions(-)",
        ],
        "{out}"
    );
}

/// `rev.diffopt.detect_rename = DIFF_DETECT_RENAME` (sequencer.c:1473): a
/// replayed rename is one file with no line changes plus a ` rename` summary
/// line, not a create/delete pair charged 3 insertions and 3 deletions.
#[test]
fn cherry_pick_summary_detects_a_replayed_rename() {
    let r = crlf_and_rename("cp-rename");
    let out = r.ok_stdout(&["cherry-pick", "main"]);
    assert_eq!(
        body(&out),
        [
            " Date: Tue Nov 14 22:13:20 2023 +0000",
            " 1 file changed, 0 insertions(+), 0 deletions(-)",
            " rename {orig => moved}/m.txt (100%)",
        ],
        "{out}"
    );
}

/// `revert` prints the same block through the same function, so it carries the
/// same two properties — a port that fixes only `cherry-pick` leaves this one
/// reporting `0 insertions(+), 0 deletions(-)`.
#[test]
fn revert_summary_counts_a_crlf_only_change() {
    let r = crlf_and_rename("rv-crlf");
    // Replay the CRLF→LF commit onto `side`, then undo it: the revert's own diff
    // is again nothing but line endings.
    r.git(&["cherry-pick", "main~1"]);
    let out = r.ok_stdout(&["revert", "--no-edit", "HEAD"]);
    assert_eq!(
        body(&out),
        [
            " Date: Tue Nov 14 22:13:20 2023 +0000",
            " 1 file changed, 3 insertions(+), 3 deletions(-)",
        ],
        "{out}"
    );
}

/// The first-bad-commit report is `git diff-tree --pretty --stat --summary --cc`
/// (bisect.c's `show_diff_tree()`), and `--stat` implies recursion — so a commit
/// that introduces a whole new directory lists the *file* inside it and nothing
/// else. The directory entries the tree walk also produces must not reach the
/// stat: `sub` is a tree object, and read as a blob it renders as
/// ` sub | Bin 0 -> NN bytes` and inflates the `N files changed` count.
#[test]
fn bisect_verdict_stat_reports_files_not_directories() {
    let r = Repo::new("bisect-verdict");
    r.write("f.txt", "base\n");
    r.git(&["add", "f.txt"]);
    r.git(&["commit", "-q", "-m", "c0"]);
    for n in ["1", "2"] {
        r.write("f.txt", &format!("{n}\n"));
        r.git(&["commit", "-qam", &format!("c{n}")]);
    }
    // The commit the bisection lands on: a new subdirectory plus an edit.
    r.write("sub/new.txt", "p\nq\n");
    r.write("f.txt", "3\n");
    r.git(&["add", "-A"]);
    r.git(&["commit", "-q", "-m", "c3 adds sub/"]);
    r.write("f.txt", "4\n");
    r.git(&["commit", "-qam", "c4"]);

    r.git(&["bisect", "start"]);
    r.git(&["bisect", "bad", "HEAD"]);
    r.git(&["bisect", "good", "HEAD~4"]);
    r.git(&["bisect", "good"]);
    let out = r.ok_stdout(&["bisect", "bad"]);

    // Everything from the blank line after the message on: the header carries
    // object ids and a `Date:` that the fixture pins but the oid does not.
    let stat: Vec<&str> = out.lines().skip_while(|l| !l.starts_with(" f.txt")).collect();
    assert_eq!(
        stat,
        [
            " f.txt       | 2 +-",
            " sub/new.txt | 2 ++",
            " 2 files changed, 3 insertions(+), 1 deletion(-)",
            " create mode 100644 sub/new.txt",
        ],
        "{out}"
    );
    assert!(
        !out.contains(" sub  ") && !out.contains("Bin"),
        "no tree entry may reach the stat: {out}"
    );

    r.git(&["bisect", "reset"]);
}
