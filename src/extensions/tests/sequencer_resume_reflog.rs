//! What each sequencer *resumption* verb writes into the reflog, pinned against
//! stock git 2.55.0.
//!
//! These strings are not decoration. `git reflog`, `@{n}` resolution, and every
//! script that greps the reflog for what an operation did read them, so a
//! resumed pick logged as `cherry-pick:` instead of `commit (cherry-pick):` is a
//! behaviour difference, not a cosmetic one. They are also the part of the
//! sequencer a single invocation can never reach: the message is written by the
//! *second* command of a two-command workflow.
//!
//! Each verb composes its message from a different place in git, which is why
//! there is a case per verb rather than one shared helper:
//!
//!   * `cherry-pick --continue` / `revert --continue` — `continue_single_pick()`
//!     (sequencer.c:5232-5257) spawns a plain `git commit` and gives it **no**
//!     `GIT_REFLOG_ACTION`, unlike `run_git_commit()` (sequencer.c:1141). The
//!     child therefore falls through to `reflog_msg`'s whence-derived default
//!     (builtin/commit.c:1850-1892), and `sequencer_determine_whence()`
//!     (sequencer.c:6847-6866) recognises `CHERRY_PICK_HEAD` **only** — so a
//!     resumed revert is a plain `commit:`.
//!   * `rebase --continue` — `commit_staged_changes()` builds
//!     `reflog_message(opts, "continue", NULL)` (sequencer.c:5267) and *does*
//!     export it (sequencer.c:5429-5430 → 1141).
//!   * `rebase --abort` — `builtin/rebase.c:1405-1408` formats
//!     `"%s (abort): returning to %s"` with the fully qualified `head_name`, or
//!     the original object id when the rebase was started detached.
//!   * `am --abort` — `builtin/am.c:2211-2215` passes the literal string
//!     `"am --abort"` to `refs_update_ref()` with `flags = 0`, so it lands on
//!     `HEAD` and, through the dereference, on the branch too.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A repository plus the hermetic `HOME` its commands run under, deleted with
/// the value.
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
        let root = std::env::temp_dir().join(format!("zvcs-seqreflog-{tag}-{}", std::process::id()));
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

    /// Run a command that is *expected* to stop (a conflict), asserting only
    /// that it did not succeed — the stop is the state the next step needs.
    fn git_stops(&self, args: &[&str]) {
        let o = self.run(args);
        assert!(!o.status.success(), "git {args:?} was supposed to stop but succeeded");
    }

    fn stdout(&self, args: &[&str]) -> String {
        String::from_utf8_lossy(&self.run(args).stdout).into_owned()
    }

    fn write(&self, rel: &str, body: &str) {
        let path = self.dir.join(rel);
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }

    fn commit(&self, rel: &str, body: &str, message: &str) {
        self.write(rel, body);
        self.git(&["add", rel]);
        self.git(&["commit", "-q", "-m", message]);
    }

    /// The reflog messages on `<refname>`, newest first.
    fn reflog(&self, refname: &str) -> Vec<String> {
        self.stdout(&["reflog", "show", refname, "--format=%gs"])
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn rev(&self, spec: &str) -> String {
        self.stdout(&["rev-parse", spec]).trim().to_string()
    }
}

/// `main` and `theirs` each add `c.txt` with different content: an add/add
/// conflict for anything that replays one onto the other.
fn add_add_conflict(tag: &str) -> Repo {
    let r = Repo::new(tag);
    r.commit("a.txt", "base\n", "base");
    r.git(&["checkout", "-q", "-b", "theirs"]);
    r.commit("c.txt", "theirs\n", "theirs");
    r.git(&["checkout", "-q", "main"]);
    r.commit("c.txt", "ours\n", "ours");
    r
}

/// `cherry-pick --continue` is a `git commit` with no `GIT_REFLOG_ACTION`, so
/// the wording is `builtin/commit.c`'s whence default — **not** the
/// `cherry-pick: <subject>` the sequencer's own in-process picks write.
///
/// The branch reflog carries the same line: the child's ref update dereferences
/// `HEAD`.
#[test]
fn cherry_pick_continue_logs_commit_cherry_pick() {
    let r = add_add_conflict("cp-continue");
    r.git_stops(&["cherry-pick", "theirs"]);
    r.write("c.txt", "theirs\n");
    r.git(&["add", "c.txt"]);
    r.git(&["cherry-pick", "--continue"]);

    assert_eq!(r.reflog("HEAD")[0], "commit (cherry-pick): theirs");
    assert_eq!(r.reflog("main")[0], "commit (cherry-pick): theirs");
}

/// `reflog_msg = getenv("GIT_REFLOG_ACTION")` is read *before* every fallback
/// (builtin/commit.c:1850), so a caller that names the action displaces the
/// whence default entirely — the mechanism `pull` and `rebase` resume through.
#[test]
fn cherry_pick_continue_honours_git_reflog_action() {
    let r = add_add_conflict("cp-continue-env");
    r.git_stops(&["cherry-pick", "theirs"]);
    r.write("c.txt", "theirs\n");
    r.git(&["add", "c.txt"]);
    let out = r
        .cmd(&["cherry-pick", "--continue"])
        .env("GIT_REFLOG_ACTION", "zzz")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    assert_eq!(r.reflog("HEAD")[0], "zzz: theirs");
}

/// A resumed **revert** logs as a plain `commit:`. `sequencer_determine_whence()`
/// looks for `CHERRY_PICK_HEAD` and nothing else, so `REVERT_HEAD` leaves
/// `whence` at `FROM_COMMIT` — while a revert that *lands* without stopping is
/// written in-process with `sequencer_reflog_action()` and does say `revert:`.
/// Both halves are asserted here because a port that hard-codes one wording gets
/// exactly one of them right.
#[test]
fn revert_continue_logs_plain_commit_but_a_landing_revert_says_revert() {
    let r = Repo::new("rv-continue");
    r.commit("f.txt", "one\n", "c1");
    r.write("f.txt", "two\n");
    r.git(&["commit", "-qam", "c2"]);
    r.write("f.txt", "three\n");
    r.git(&["commit", "-qam", "c3"]);

    // Reverting c2 (two -> one) against a worktree at "three" conflicts.
    r.git_stops(&["revert", "--no-edit", "HEAD~1"]);
    r.write("f.txt", "one\n");
    r.git(&["add", "f.txt"]);
    r.git(&["revert", "--continue"]);
    assert_eq!(r.reflog("HEAD")[0], "commit: Revert \"c2\"");

    // The unstopped form, for contrast: reverting the commit just made applies
    // cleanly and takes the sequencer's own wording. (The subject is `Reapply`
    // rather than `Revert "Revert …"` because `sequencer.c`'s message builder
    // collapses a double revert.)
    r.git(&["revert", "--no-edit", "HEAD"]);
    assert_eq!(r.reflog("HEAD")[0], "revert: Reapply \"c2\"");
}

/// `rebase --continue` exports `"<action> (continue)"` to the `git commit` it
/// runs, so the resumed pick is `rebase (continue): <subject>` — distinguishable
/// in the reflog from both an ordinary `commit:` and an unstopped
/// `rebase (pick):`.
#[test]
fn rebase_continue_logs_rebase_continue() {
    let r = add_add_conflict("rb-continue");
    r.git_stops(&["rebase", "theirs"]);
    // The replayed side of the conflict is `ours`; staging it makes the resumed
    // commit a real change rather than a no-op the rebase would drop.
    r.write("c.txt", "ours\n");
    r.git(&["add", "c.txt"]);
    r.git(&["rebase", "--continue"]);

    let log = r.reflog("HEAD");
    assert_eq!(
        &log[..3],
        [
            "rebase (finish): returning to refs/heads/main",
            "rebase (continue): ours",
            "rebase (start): checkout theirs",
        ],
        "reflog: {log:?}"
    );
}

/// `rebase --abort` names the ref it is returning to. Truncating the message to
/// `rebase (abort): returning` loses the only record of where the aborted rebase
/// put `HEAD`.
#[test]
fn rebase_abort_names_the_branch_it_returns_to() {
    let r = add_add_conflict("rb-abort");
    r.git_stops(&["rebase", "-i", "theirs"]);
    r.git(&["rebase", "--abort"]);

    assert_eq!(r.reflog("HEAD")[0], "rebase (abort): returning to refs/heads/main");
}

/// Started detached, there is no `head_name`, and git prints the original object
/// id in full instead — `oid_to_hex(&options.orig_head->object.oid)`, not an
/// abbreviation and not nothing.
#[test]
fn rebase_abort_detached_names_the_original_commit() {
    let r = add_add_conflict("rb-abort-detached");
    r.git(&["checkout", "-q", "--detach", "main"]);
    let orig = r.rev("HEAD");
    r.git_stops(&["rebase", "-i", "theirs"]);
    r.git(&["rebase", "--abort"]);

    assert_eq!(r.reflog("HEAD")[0], format!("rebase (abort): returning to {orig}"));
}

/// `am --abort` writes the literal `am --abort` on both `HEAD` and the branch,
/// and — the part that is not a reflog detail at all — leaves `ORIG_HEAD` alone.
///
/// Delegating the rewind to `git reset --hard ORIG_HEAD` gets both wrong: the
/// line reads `reset: moving to ORIG_HEAD`, and `reset` *rewrites* `ORIG_HEAD`
/// to the HEAD it is leaving — so aborting an `am` that had already applied one
/// patch left `ORIG_HEAD` pointing at the half-applied tip instead of at the
/// commit the session started from. The two-patch mailbox below is what makes
/// that visible; a mailbox that fails on its first patch never moves `HEAD` and
/// hides it.
#[test]
fn am_abort_logs_am_abort_and_preserves_orig_head() {
    let r = Repo::new("am-abort");
    r.commit("f.txt", "one\n", "base");
    r.git(&["checkout", "-q", "-b", "feat"]);
    r.commit("g.txt", "two\n", "p1 add g");
    r.write("f.txt", "three\n");
    r.git(&["commit", "-qam", "p2 change f"]);
    r.git(&["checkout", "-q", "main"]);
    r.write("f.txt", "conflicting\n");
    r.git(&["commit", "-qam", "main change f"]);
    let before = r.rev("HEAD");

    r.git(&["format-patch", "-q", "-2", "feat", "-o", "mail"]);
    // Patch 1 applies, patch 2 fails against `main`'s own edit to `f.txt`.
    r.git_stops(&["am", "mail/0001-p1-add-g.patch", "mail/0002-p2-change-f.patch"]);
    assert_ne!(r.rev("HEAD"), before, "the first patch should have landed");

    r.git(&["am", "--abort"]);
    assert_eq!(r.reflog("HEAD")[0], "am --abort");
    assert_eq!(r.reflog("main")[0], "am --abort");
    assert_eq!(r.rev("HEAD"), before, "HEAD must be back where the am started");
    assert_eq!(r.rev("ORIG_HEAD"), before, "am --abort must not rewrite ORIG_HEAD");
    assert!(!Path::new(&r.dir).join(".git/rebase-apply").exists());
}
