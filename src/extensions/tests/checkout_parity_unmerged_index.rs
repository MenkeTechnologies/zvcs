//! An unmerged index refuses every switch, and `restore --staged` over one writes
//! a well-formed index.
//!
//! **The gate.** `merge_working_tree()` opens the non-forced path with
//!
//! ```c
//! refresh_index(the_repository->index, REFRESH_QUIET, NULL, NULL, NULL);
//!
//! if (unmerged_index(the_repository->index)) {
//!         rollback_lock_file(&lock_file);
//!         error(_("you need to resolve your current index first"));
//!         return 1;
//! }
//! ```
//!
//! (builtin/checkout.c:883-889) — ahead of the two-way unpack, so the refusal is
//! that one and not `unpack_trees()`'s "Your local changes to the following files
//! would be overwritten by checkout". `switch_branches()` reaches it for every
//! switch, `--detach` and `--orphan` included; the port only had it on the plain
//! branch switch, so those two ran the unpack instead and either refused with the
//! wrong message or (when the target tree equalled `HEAD`'s, as `--orphan` with no
//! start point and `--detach` with no operand both are) moved `HEAD` over an
//! unresolved conflict.
//!
//! The one spelling that legitimately skips it is `git checkout -b <branch>` with
//! exactly those three argv words:
//!
//! ```c
//! if (argc == 3 && !strcmp(argv[1], "-b")) {
//!         opts.switch_branch_doing_nothing_is_ok = 0;
//!         opts.only_merge_on_switching_branches = 1;
//! }
//! ```
//!
//! (builtin/checkout.c:2123-2130), which clears `do_merge` (:1202-1203) and skips
//! `merge_working_tree()` entirely. Adding `-q`, a start point, or any other flag
//! puts the gate back.
//!
//! **The index.** `git restore --staged <pathspec>` over an unmerged index drops
//! the conflict's stages and writes the source's stage-0 entry. `add_index_entry()`
//! keeps the index sorted at every step (read-cache.c); the port appended and then
//! looked later paths up with a binary search over the now-unsorted array, so a
//! path could be reported missing and appended twice. The set it iterated came from
//! a `HashSet`, so the corruption appeared in roughly half the runs — a stock read
//! of the result then showed a phantom added path.
//!
//! Every expectation below was measured against stock git 2.55.0 first.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    base: PathBuf,
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

impl Fixture {
    /// `a.txt` left conflicted by a merge of `other` into `main`; `k.txt` clean and
    /// identical on both sides.
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!("zvcs-coum-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("repo");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(base.join("home")).unwrap();
        let f = Fixture { base, root };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.write("a.txt", "l1\nl2\nl3\n");
        f.write("k.txt", "base\n");
        f.git(&["add", "-A"]);
        f.git(&["commit", "-qm", "base"]);
        f.git(&["branch", "other"]);
        f.write("a.txt", "main1\nl2\nl3\n");
        f.git(&["commit", "-qam", "ours"]);
        f.git(&["checkout", "-q", "other"]);
        f.write("a.txt", "other1\nl2\nl3\n");
        f.git(&["commit", "-qam", "theirs"]);
        f.git(&["checkout", "-q", "main"]);
        f.git(&["merge", "other"]);
        f
    }

    fn write(&self, rel: &str, body: &str) {
        std::fs::write(self.root.join(rel), body).unwrap();
    }

    fn git(&self, args: &[&str]) -> (String, i32) {
        let out = Command::new(BIN)
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .env("HOME", self.base.join("home"))
            .env("ZVCS_HOME", self.base.join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "2023-01-01 00:00:00 +0000")
            .env("GIT_COMMITTER_DATE", "2023-01-01 00:00:00 +0000")
            .output()
            .unwrap();
        let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
        s.push_str(&String::from_utf8_lossy(&out.stderr));
        (s, out.status.code().unwrap_or(-1))
    }

    fn head(&self) -> String {
        std::fs::read_to_string(self.root.join(".git/HEAD")).unwrap()
    }

    fn assert_refused(&self, args: &[&str]) {
        let (out, rc) = self.git(args);
        assert_eq!(rc, 1, "{args:?} must exit 1, got: {out}");
        assert!(
            out.contains("error: you need to resolve your current index first"),
            "{args:?} must hit merge_working_tree()'s first gate, got: {out:?}"
        );
        assert!(
            out.contains("a.txt: needs merge"),
            "refresh_index() names each unmerged path for {args:?}, got: {out:?}"
        );
        assert!(
            !out.contains("would be overwritten"),
            "the unpack's dirty-file refusal must not be what {args:?} reports: {out:?}"
        );
        assert_eq!(
            self.head(),
            "ref: refs/heads/main\n",
            "HEAD must not move for {args:?}"
        );
    }
}

#[test]
fn detaching_over_an_unmerged_index_is_refused() {
    let f = Fixture::new("detach");
    // With an operand the port reported the unpack's dirty-file error; with none the
    // target tree equals HEAD's, so nothing refused at all and HEAD detached.
    f.assert_refused(&["checkout", "--detach", "other"]);
    f.assert_refused(&["checkout", "--detach"]);

    // `switch` never reaches that gate here: `opts.can_switch_when_in_progress = 0`
    // (builtin/checkout.c:2166) refuses an in-progress merge first, at 128.
    let (out, rc) = f.git(&["switch", "--detach", "other"]);
    assert_eq!(rc, 128, "switch refuses mid-merge before the index gate, got: {out}");
    assert_eq!(
        out,
        "fatal: cannot switch branch while merging\n\
         Consider \"git merge --quit\" or \"git worktree add\".\n",
        "and with its own wording: {out:?}"
    );
}

#[test]
fn starting_an_orphan_over_an_unmerged_index_is_refused() {
    let f = Fixture::new("orphan");
    f.assert_refused(&["checkout", "--orphan", "o"]);
    f.assert_refused(&["checkout", "--orphan", "o", "other"]);
}

#[test]
fn only_the_bare_three_word_dash_b_skips_the_gate() {
    let f = Fixture::new("dashb");
    // `argc == 3 && !strcmp(argv[1], "-b")` — this exact spelling and no other.
    let (out, rc) = f.git(&["checkout", "-b", "nb"]);
    assert_eq!(rc, 0, "`git checkout -b <branch>` skips merge_working_tree(): {out}");
    assert_eq!(out, "Switched to a new branch 'nb'\n", "and says only that: {out:?}");
    assert_eq!(f.head(), "ref: refs/heads/nb\n", "HEAD must move to the new branch");
    // The conflict is carried over untouched, which is the point of skipping.
    let (stage, _) = f.git(&["ls-files", "--stage", "--", "a.txt"]);
    assert_eq!(stage.lines().count(), 3, "all three stages survive: {stage:?}");

    // Any other spelling of the same intent keeps the gate.
    let g = Fixture::new("dashb2");
    g.assert_refused(&["checkout", "-q", "-b", "nb"]);
    let h = Fixture::new("dashb3");
    h.assert_refused(&["checkout", "-b", "nb", "other"]);
    let i = Fixture::new("dashb4");
    i.assert_refused(&["checkout", "-B", "other"]);
}

#[test]
fn a_forced_switch_still_escapes_the_conflict() {
    let f = Fixture::new("force");
    // `opts->discard_changes` takes `reset_tree()` and never reaches the gate
    // (builtin/checkout.c:871-876), which is what makes `-f` the way out.
    let (out, rc) = f.git(&["checkout", "-f", "other"]);
    assert_eq!(rc, 0, "a forced switch must succeed, got: {out}");
    assert_eq!(f.head(), "ref: refs/heads/other\n", "HEAD must move");
    let (stage, _) = f.git(&["ls-files", "--stage", "--", "a.txt"]);
    assert_eq!(stage.lines().count(), 1, "the conflict is gone: {stage:?}");
}

#[test]
fn restore_staged_over_an_unmerged_index_writes_each_path_once() {
    // The corruption was ordering-dependent, so this repeats; a single run caught it
    // about half the time.
    for round in 0..4 {
        let f = Fixture::new(&format!("staged{round}"));
        let (out, rc) = f.git(&["restore", "--staged", "."]);
        assert_eq!(rc, 0, "restore --staged . should succeed, got: {out}");

        let (stage, _) = f.git(&["ls-files", "--stage"]);
        let paths: Vec<&str> = stage.lines().filter_map(|l| l.split_once('\t').map(|(_, p)| p)).collect();
        assert_eq!(
            paths,
            vec!["a.txt", "k.txt"],
            "round {round}: one stage-0 entry per path, in order: {stage:?}"
        );

        // A duplicate shows up in a fresh read of the index as a phantom addition.
        let (status, _) = f.git(&["status", "--porcelain=v2"]);
        assert!(
            !status.lines().any(|l| l.starts_with("1 A. ")),
            "round {round}: nothing was added, so nothing may be reported as added: {status:?}"
        );
    }
}
