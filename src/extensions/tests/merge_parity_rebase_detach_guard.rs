//! The `unpack_trees()` gate every rebase start goes through before it detaches.
//!
//! Both starts call `reset_head()` with `RESET_HEAD_DETACH | RESET_ORIG_HEAD` —
//! `checkout_onto()` (sequencer.c:4866-4886) for the merge backend,
//! builtin/rebase.c:1875-1886 for the apply one — and `reset_head()` is an
//! `unpack_trees()` two-tree `twoway_merge` with `.update = 1` and
//! `setup_unpack_trees_porcelain(…, "checkout")` (reset.c:131-164). So an
//! untracked file standing where `<onto>` wants one, or a local change the move
//! would lose, stops the rebase with git's checkout refusal before anything is
//! written:
//!
//! ```c
//! if (reset_head(r, &ropts)) {
//!         apply_autostash(rebase_path_autostash());
//!         sequencer_remove_state(opts);
//!         return error(_("could not detach HEAD"));
//! }
//! ```
//!
//! Without the gate the rebase overwrote the file and reported success. Two
//! details matter for the shape of the refusal:
//!
//! * the left-hand tree is the one the *current* `HEAD` names (reset.c:118,
//!   :150), not the branch being rebased — `git rebase <upstream> <branch>`
//!   checks `<branch>` out only when it can fast-forward
//!   (builtin/rebase.c:1804-1810);
//! * `reset_head()` runs before `ORIG_HEAD` is written and before any state
//!   directory is created, so a refused start leaves `git rebase --quit`
//!   saying `no rebase in progress`.
//!
//! Every expectation was measured from stock git 2.55.0 in an identical
//! throwaway repository under the same pinned environment, stdout, stderr and
//! exit status compared separately.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

const UNTRACKED_REFUSAL: &str =
    "error: The following untracked working tree files would be overwritten by checkout:\n\
     \tB\n\
     Please move or remove them before you switch branches.\n\
     Aborting\n";

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
    /// `main` and `topic` diverged off one root: `topic` adds `B`, `main`
    /// rewrites `A`. Rebasing `main` onto `topic` therefore has to write `B`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-reb-detach-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("A"), "A\n").unwrap();
        f.git(&["add", "A"]);
        f.git(&["commit", "-q", "-m", "A"]);
        f.git(&["checkout", "-q", "-b", "topic"]);
        std::fs::write(f.work.join("B"), "B\n").unwrap();
        f.git(&["add", "B"]);
        f.git(&["commit", "-q", "-m", "B"]);
        f.git(&["checkout", "-q", "main"]);
        std::fs::write(f.work.join("A"), "A\nThird\n").unwrap();
        f.git(&["commit", "-q", "-a", "-m", "modify"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_SEQUENCE_EDITOR", ":")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
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

    fn oid(&self, spec: &str) -> String {
        self.stdout(&["rev-parse", spec]).trim().to_string()
    }
}

/// Every backend refuses, and only the apply backend's wording differs
/// (`Could not detach HEAD` against the sequencer's lowercase `could`).
#[test]
fn an_untracked_file_in_the_way_stops_every_rebase_backend() {
    // The apply backend announces the rewind on stdout before it tries the
    // detach (builtin/rebase.c:1871-1873), so the refused run still prints it.
    for (flag, tail, want_out) in [
        ("", "error: could not detach HEAD\n", ""),
        ("--merge", "error: could not detach HEAD\n", ""),
        ("-i", "error: could not detach HEAD\n", ""),
        (
            "--apply",
            "error: Could not detach HEAD\n",
            "First, rewinding head to replay your work on top of it...\n",
        ),
    ] {
        let f = Fixture::new(&format!("backend{}", flag.trim_start_matches('-')));
        let before = f.oid("HEAD");
        std::fs::write(f.work.join("B"), "untracked\n").unwrap();

        let mut args = vec!["rebase"];
        if !flag.is_empty() {
            args.push(flag);
        }
        args.push("topic");
        let (out, err, code) = f.run(&args);
        assert_eq!((out.as_str(), code), (want_out, 1), "`git {args:?}`: {err}");
        assert_eq!(err, format!("{UNTRACKED_REFUSAL}{tail}"), "`git {args:?}`");

        // Nothing moved and nothing was destroyed.
        assert_eq!(std::fs::read_to_string(f.work.join("B")).unwrap(), "untracked\n");
        assert_eq!(f.oid("HEAD"), before);
        assert_eq!(f.stdout(&["rev-parse", "--abbrev-ref", "HEAD"]).trim(), "main");
        let (_, _, code) = f.run(&["rev-parse", "--verify", "-q", "ORIG_HEAD"]);
        assert_eq!(code, 1, "ORIG_HEAD was written by a refused start");
        let (_, err, code) = f.run(&["rebase", "--quit"]);
        assert_eq!((err.as_str(), code), ("fatal: no rebase in progress\n", 128));
    }
}

/// The gate reads the tree of the `HEAD` that is actually checked out. With a
/// `<branch>` operand that is not current and cannot fast-forward, git never
/// switches to it (builtin/rebase.c:1804-1810), so comparing against the
/// branch's tree instead would refuse rebases stock performs.
#[test]
fn the_gate_compares_against_the_checked_out_head_not_the_branch_operand() {
    let f = Fixture::new("operand");
    // `side` is a third line of history whose tree has neither `B` nor the
    // modified `A`; it is checked out while `main` is the branch to rebase.
    f.git(&["checkout", "-q", "-b", "side", "main~1"]);
    std::fs::write(f.work.join("C"), "C\n").unwrap();
    f.git(&["add", "C"]);
    f.git(&["commit", "-q", "-m", "C"]);
    let side = f.oid("HEAD");

    // `B` exists only in `topic`, and `side`'s worktree does not hold it — so
    // nothing is in the way and the rebase runs.
    let (_, err, code) = f.run(&["rebase", "topic", "main"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(f.stdout(&["rev-parse", "--abbrev-ref", "HEAD"]).trim(), "main");
    assert_eq!(f.oid("main~1"), f.oid("topic"));
    assert_ne!(f.oid("main"), side);
}

/// A *staged* change never reaches the gate: `require_clean_work_tree()`
/// (builtin/rebase.c) turns it away first with its own wording, so the untracked
/// case above is the only one the detach itself has left to catch. Pinned so a
/// future change to either check cannot silently swap the two diagnoses.
#[test]
fn a_dirty_index_is_refused_before_the_detach_gate_is_reached() {
    let f = Fixture::new("staged");
    let before = f.oid("HEAD");
    std::fs::write(f.work.join("B"), "staged\n").unwrap();
    f.git(&["add", "B"]);

    let (out, err, code) = f.run(&["rebase", "topic"]);
    assert_eq!((out.as_str(), code), ("", 1), "{err}");
    assert_eq!(
        err,
        "error: cannot rebase: Your index contains uncommitted changes.\n\
         error: Please commit or stash them.\n"
    );
    assert_eq!(std::fs::read_to_string(f.work.join("B")).unwrap(), "staged\n");
    assert_eq!(f.oid("HEAD"), before);
}
