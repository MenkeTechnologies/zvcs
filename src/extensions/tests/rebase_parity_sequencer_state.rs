//! Five `git rebase` divergences from stock 2.55.0, each measured on the same
//! fixture under `/usr/local/bin/git` before the expectation was written here.
//!
//! * The six action switches (`--continue`, `--skip`, `--abort`, `--quit`,
//!   `--edit-todo`, `--show-current-patch`) are `OPT_CMDMODE`s sharing one slot
//!   (`builtin/rebase.c:1167-1180`). A second, *different* one is a
//!   `parse-options` error naming both (`parse-options.c:417-420`); the port
//!   silently overwrote the slot and fell through to the generic usage block.
//! * `die()`ing on an existing state directory quotes `options.state_dir`, a
//!   `GIT_PATH_FUNC` (`builtin/rebase.c:53-54, 1459-1468`), so an ordinary
//!   repository spells it `.git/rebase-merge` however deep the command was
//!   typed; the port printed a path relative to the cwd. Its format string also
//!   ends in `\n` on top of `die()`'s, so a blank line follows.
//! * Neither `rebase-merge` nor `rebase-apply` is in `path.c`'s `common_list`
//!   (`path.c:98-124`), so rebase state is per-worktree. The port looked in the
//!   common directory and so never noticed a rebase in progress in a linked
//!   worktree — while writing its own state to the per-worktree directory.
//! * `do_merge()` skips a merge head that is already an ancestor of `HEAD`
//!   (`sequencer.c:4334-4339`). The port merged it anyway and wrote a
//!   two-parent commit stock never creates.
//! * `label`/`reset`/`merge`/`update-ref` failures set `reschedule = 1`
//!   (`sequencer.c:5078-5117`): the instruction goes back on the sheet, the
//!   `Could not execute the todo command` advice quotes it, and `lookup_label()`
//!   names the bare label because it splices `refs/rewritten/` back off
//!   (`sequencer.c:3997-4002`). The port reported the full ref, printed no
//!   advice, and left `git-rebase-todo` empty — so `--continue` concluded the
//!   rebase and discarded every instruction still queued behind the failure.
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
    /// `main` = base, m1; `topic` = base, ta, tb, tc. `topic` is checked out.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-rbseq-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.write("f.txt", "base\n");
        f.git(&["add", "f.txt"]);
        f.git(&["commit", "-q", "-m", "base"]);
        f.git(&["checkout", "-q", "-b", "topic"]);
        for n in ["ta", "tb", "tc"] {
            f.write(&format!("{n}.txt"), n);
            f.git(&["add", "."]);
            f.git(&["commit", "-q", "-m", n]);
        }
        f.git(&["checkout", "-q", "main"]);
        f.write("m.txt", "m\n");
        f.git(&["add", "."]);
        f.git(&["commit", "-q", "-m", "m1"]);
        f.git(&["checkout", "-q", "topic"]);
        f
    }

    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.work.join(name), body).unwrap();
    }

    fn cmd_in(&self, dir: &PathBuf, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_SEQUENCE_EDITOR", ":")
            .env("GIT_EDITOR", ":")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@e.co")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@e.co")
            .env("GIT_AUTHOR_DATE", "2005-04-07T15:13:13-07:00")
            .env("GIT_COMMITTER_DATE", "2005-04-07T15:13:13-07:00");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd_in(&self.work.clone(), args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    fn run_in(&self, dir: &PathBuf, args: &[&str]) -> (i32, String, String) {
        let out = self.cmd_in(dir, args).output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn run(&self, args: &[&str]) -> (i32, String, String) {
        self.run_in(&self.work.clone(), args)
    }

    fn stdout(&self, args: &[&str]) -> String {
        let out = self.cmd_in(&self.work.clone(), args).output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn subjects(&self) -> Vec<String> {
        self.stdout(&["log", "--format=%s", "-n", "8"])
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn state(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.work.join(".git/rebase-merge").join(name)).ok()
    }

    /// Drive `rebase -i` with a hand-written sheet: the "editor" replaces
    /// whatever was generated with `body`.
    fn with_sheet(&self, body: &str, args: &[&str]) -> (i32, String, String) {
        let sheet = self.root.join("sheet");
        std::fs::write(&sheet, body).unwrap();
        let ed = self.root.join("ed.sh");
        std::fs::write(
            &ed,
            format!("#!/bin/sh\ncat {} > \"$1\"\n", sheet.display()),
        )
        .unwrap();
        let mut perms = std::fs::metadata(&ed).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&ed, perms).unwrap();
        let out = self
            .cmd_in(&self.work.clone(), args)
            .env("GIT_SEQUENCE_EDITOR", ed.display().to_string())
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

/// `git rebase --continue --abort` is a `parse-options` cmdmode conflict, not a
/// usage error: one `error:` line naming the switch being parsed first and the
/// stored one second, no usage block, status 129.
///
/// Repeating the *same* switch is accepted (`parse-options.c:401`) and then
/// fails the later `argc != 2` check, which is the usage block — so the two
/// halves of this test pin opposite behaviours and a port that conflates them
/// cannot pass both.
#[test]
fn action_switches_conflict_as_cmdmodes() {
    let f = Fixture::new("cmdmode");

    let (code, out, err) = f.run(&["rebase", "--continue", "--abort"]);
    assert_eq!(code, 129, "stdout={out:?} stderr={err:?}");
    assert_eq!(
        err, "error: options '--abort' and '--continue' cannot be used together\n",
        "conflict must be the whole diagnostic, with no usage block"
    );

    // Reversed order names the pair the other way round.
    let (code, _, err) = f.run(&["rebase", "--abort", "--continue"]);
    assert_eq!(code, 129);
    assert_eq!(
        err,
        "error: options '--continue' and '--abort' cannot be used together\n"
    );

    // Same switch twice is not a conflict; it reaches the usage block instead.
    let (code, _, err) = f.run(&["rebase", "--continue", "--continue"]);
    assert_eq!(code, 129);
    assert!(
        !err.contains("cannot be used together"),
        "repeating one switch is accepted by parse-options: {err:?}"
    );
    assert!(err.starts_with("usage: git rebase "), "{err:?}");
}

/// The "already a rebase-merge directory" refusal quotes `$GIT_DIR` as `.git`
/// from anywhere in the work tree, and is followed by a blank line.
#[test]
fn existing_state_dir_refusal_quotes_dot_git_and_ends_blank() {
    let f = Fixture::new("statedir");
    std::fs::create_dir_all(f.work.join(".git/rebase-merge")).unwrap();

    let expected = "fatal: It seems that there is already a rebase-merge directory, and\n\
         I wonder if you are in the middle of another rebase.  If that is the\n\
         case, please try\n\tgit rebase (--continue | --abort | --skip)\n\
         If that is not the case, please\n\trm -fr \".git/rebase-merge\"\n\
         and run me again.  I am stopping in case you still have something\n\
         valuable there.\n\n";

    let (code, _, err) = f.run(&["rebase", "main"]);
    assert_eq!(code, 128);
    assert_eq!(err, expected);

    // git moves to the top of the work tree before printing, so a command typed
    // two directories down says exactly the same thing.
    let deep = f.work.join("a/b");
    std::fs::create_dir_all(&deep).unwrap();
    let (code, _, err) = f.run_in(&deep, &["rebase", "main"]);
    assert_eq!(code, 128);
    assert_eq!(err, expected, "state dir must not be spelled relative to cwd");
}

/// A linked worktree keeps its own `rebase-merge`, so a rebase in progress there
/// must be seen from there — and named by its full path, since a worktree's
/// `$GIT_DIR` has no `.git` shorthand.
#[test]
fn linked_worktree_rebase_state_is_per_worktree() {
    let f = Fixture::new("worktree");
    let wt = f.root.join("wt");
    // `topic` is checked out here, so the worktree gets a branch of its own.
    f.git(&["worktree", "add", "-q", "-b", "side", wt.to_str().unwrap(), "topic"]);

    let state = f.work.join(".git/worktrees/wt/rebase-merge");
    std::fs::create_dir_all(&state).unwrap();

    // The common directory has no rebase state, so a port reading it sees none.
    assert!(!f.work.join(".git/rebase-merge").exists());

    let (code, _, err) = f.run_in(&wt, &["rebase", "main"]);
    assert_eq!(code, 128, "in-progress rebase in this worktree was missed: {err:?}");
    // A worktree's `$GIT_DIR` has no `.git` shorthand, so the path is absolute.
    // `std::env::temp_dir()` is reached through a symlink on macOS, so compare
    // the resolved form rather than the one the fixture built.
    let resolved = std::fs::canonicalize(&state).unwrap();
    assert!(
        err.contains(&format!("rm -fr \"{}\"", resolved.display())),
        "must name this worktree's own state directory ({}): {err:?}",
        resolved.display()
    );
}

/// `merge <label>` where the label is already an ancestor of `HEAD` records no
/// merge at all: `HEAD` does not move and no two-parent commit is written.
#[test]
fn merge_instruction_skips_an_ancestor_of_head() {
    let f = Fixture::new("ancestor");
    let onto = f.stdout(&["rev-parse", "main"]).trim().to_string();
    let orig = f.stdout(&["rev-parse", "topic"]).trim().to_string();

    // The sheet's only instruction merges `main` while `HEAD` is already `main`
    // (the rebase checks out `onto` first), so the merge head is the merge base.
    let (code, out, err) = f.with_sheet(
        &format!("merge -C {orig} main\n"),
        &["rebase", "-i", "main"],
    );
    assert_eq!(code, 0, "stdout={out:?} stderr={err:?}");

    assert_eq!(
        f.stdout(&["rev-parse", "HEAD"]).trim(),
        onto,
        "HEAD must stay on the commit the rebase checked out"
    );
    assert_eq!(
        f.stdout(&["rev-list", "--count", "--merges", "HEAD"]).trim(),
        "0",
        "no merge commit may be created for an ancestor merge head"
    );
    assert_eq!(f.subjects(), vec!["m1".to_string(), "base".to_string()]);
}

/// An unresolvable `reset` label reschedules: the bare label is named, the
/// advice block quotes the whole instruction, and everything queued behind the
/// failure survives in `git-rebase-todo` so `--continue` still runs it.
#[test]
fn unresolvable_reset_label_reschedules_and_keeps_the_sheet() {
    let f = Fixture::new("reschedule");

    let (code, _, err) = f.with_sheet(
        "label keep\nreset nosuchlabel\nreset keep\n",
        &["rebase", "-i", "main"],
    );
    assert_eq!(code, 1, "{err:?}");
    assert!(
        err.contains("error: could not resolve 'nosuchlabel'\n"),
        "must name the label as written, not the refs/rewritten/ ref tried first: {err:?}"
    );
    assert!(
        !err.contains("refs/rewritten/nosuchlabel"),
        "the refs/rewritten/ spelling is spliced off before the diagnostic: {err:?}"
    );
    assert!(
        err.contains(
            "hint: Could not execute the todo command\nhint:\nhint:     reset nosuchlabel\n"
        ),
        "the advice must quote the whole rescheduled instruction: {err:?}"
    );
    assert!(err.contains("hint:     git rebase --edit-todo\n"), "{err:?}");

    // The failed instruction is back at the head of the sheet, with the
    // instruction that followed it still behind.
    assert_eq!(
        f.state("git-rebase-todo").as_deref(),
        Some("reset nosuchlabel\nreset keep\n"),
        "a dropped reschedule silently discards the rest of the sheet"
    );
    assert_eq!(f.state("done").as_deref(), Some("label keep\nreset nosuchlabel\n"));

    // `--continue` retries the same instruction rather than concluding the
    // rebase, so it fails the same way instead of exiting 0.
    let (code, _, err) = f.run(&["rebase", "--continue"]);
    assert_eq!(code, 1, "--continue must retry the rescheduled instruction: {err:?}");
    assert!(err.contains("error: could not resolve 'nosuchlabel'"), "{err:?}");
}

/// A `merge` whose label will not resolve costs two `error:` lines — the
/// `lookup_label()` failure and `do_merge()`'s own — and then reschedules,
/// writing `REBASE_HEAD` because the instruction named a commit.
#[test]
fn unresolvable_merge_label_reports_both_errors() {
    let f = Fixture::new("mergelabel");
    let orig = f.stdout(&["rev-parse", "topic"]).trim().to_string();

    let (code, _, err) =
        f.with_sheet(&format!("merge -C {orig} nosuch\n"), &["rebase", "-i", "main"]);
    assert_eq!(code, 1, "{err:?}");
    assert!(
        err.contains("error: could not resolve 'nosuch'\nerror: unable to parse 'nosuch'\n"),
        "both diagnostics, in this order: {err:?}"
    );
    assert!(err.contains("hint: Could not execute the todo command"), "{err:?}");
    assert_eq!(
        f.state("git-rebase-todo").as_deref(),
        Some(format!("merge -C {orig} nosuch\n").as_str())
    );
    assert_eq!(
        std::fs::read_to_string(f.work.join(".git/REBASE_HEAD"))
            .unwrap()
            .trim(),
        orig,
        "an instruction carrying a commit writes REBASE_HEAD when rescheduled"
    );
}
