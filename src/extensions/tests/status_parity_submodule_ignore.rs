//! Which parts of a dirty submodule `git status` names, and who gets to decide.
//!
//! `wt_status_collect_changes_worktree()` (wt-status.c:646-655) builds the flag
//! word in three steps, and the order is the whole behaviour:
//!
//! ```c
//! rev.diffopt.flags.dirty_submodules = 1;
//! if (!s->show_untracked_files)
//!         rev.diffopt.flags.ignore_untracked_in_submodules = 1;
//! if (s->ignore_submodule_arg) {
//!         rev.diffopt.flags.override_submodule_config = 1;
//!         handle_ignore_submodules_arg(&rev.diffopt, s->ignore_submodule_arg);
//! } else if (!rev.diffopt.flags.ignore_submodule_set &&
//!                 s->show_untracked_files != SHOW_NO_UNTRACKED_FILES)
//!         handle_ignore_submodules_arg(&rev.diffopt, "none");
//! ```
//!
//! `handle_ignore_submodules_arg()` (submodule.c:429-441) *clears* all three
//! ignore bits before setting the one its argument names, and
//! `set_diffopt_flags_from_submodule_config()` (submodule.c:180-199) calls it once
//! more per path for `submodule.<name>.ignore` unless `override_submodule_config`
//! is set. So the precedence is: `--ignore-submodules=<when>`, then
//! `submodule.<name>.ignore`, then `-uno`'s lone untracked bit, then
//! `diff.ignoreSubmodules`.
//!
//! The classification itself comes from `is_submodule_modified()`
//! (submodule.c:1880), which reads *every* line of the submodule's own status to
//! build `d->dirty_submodule` — stopping at the first change loses whichever of
//! `DIRTY_SUBMODULE_MODIFIED` / `DIRTY_SUBMODULE_UNTRACKED` came second.
//!
//! Every expectation was measured from stock git 2.55.0 in an identical
//! throwaway repository under the same pinned environment.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

const BOTH: &str = "\tmodified:   sm (modified content, untracked content)\n";
const MODIFIED_ONLY: &str = "\tmodified:   sm (modified content)\n";

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
    /// A superproject whose one submodule `sm` is dirty in both ways at once —
    /// a tracked file modified and an untracked file present — with its recorded
    /// commit unchanged, so the only thing under test is the pair of dirty bits.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-st-smig-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        let origin = root.join("origin");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&origin).unwrap();
        let f = Fixture { root, work };

        f.run_in(&origin, &["init", "-q", "-b", "main", "."]);
        std::fs::write(origin.join("s"), "one\n").unwrap();
        f.run_in(&origin, &["add", "s"]);
        f.run_in(&origin, &["commit", "-q", "-m", "one"]);

        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.git(&["add", "a"]);
        f.git(&["commit", "-q", "-m", "root"]);
        f.git(&[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            origin.to_str().unwrap(),
            "sm",
        ]);
        f.git(&["commit", "-q", "-m", "add submodule"]);

        let sm = f.work.join("sm");
        std::fs::write(sm.join("s"), "one\ntwo\n").unwrap();
        std::fs::write(sm.join("untracked-here"), "u\n").unwrap();
        f
    }

    fn cmd_in(&self, dir: &std::path::Path, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("ZVCS_HOME", &self.root)
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
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        c
    }

    fn run_in(&self, dir: &std::path::Path, args: &[&str]) {
        let out = self.cmd_in(dir, args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn git(&self, args: &[&str]) {
        self.run_in(&self.work.clone(), args);
    }

    /// The one line of the long report that names the submodule, or the empty
    /// string when the report does not mention it at all.
    fn submodule_line(&self, args: &[&str]) -> String {
        let out = self.cmd_in(&self.work.clone(), args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find(|l| l.contains("modified:   sm"))
            .map(|l| format!("{l}\n"))
            .unwrap_or_default()
    }
}

/// The baseline both other tests are measured against: with nothing configured
/// and nothing asked for, both dirty bits are reported.
#[test]
fn an_unconfigured_report_names_both_kinds_of_dirt() {
    let f = Fixture::new("plain");
    assert_eq!(f.submodule_line(&["status"]), BOTH);
    assert_eq!(f.submodule_line(&["status", "-uall"]), BOTH);
}

/// `if (!s->show_untracked_files) ignore_untracked_in_submodules = 1` — `-uno`
/// suppresses untracked files *inside* the submodule, not only at top level.
#[test]
fn untracked_files_no_also_silences_untracked_content_in_a_submodule() {
    let f = Fixture::new("uno");
    assert_eq!(f.submodule_line(&["status", "-uno"]), MODIFIED_ONLY);
    assert_eq!(
        f.submodule_line(&["-c", "status.showUntrackedFiles=no", "status"]),
        MODIFIED_ONLY
    );
}

/// `handle_ignore_submodules_arg()` zeroes every ignore bit before applying its
/// own, so an explicit `--ignore-submodules=none` takes back the bit `-uno` had
/// just set — and, being `override_submodule_config`, also the one
/// `submodule.<name>.ignore` would have set.
#[test]
fn an_explicit_none_re_enables_untracked_content_that_was_suppressed() {
    let f = Fixture::new("none");
    assert_eq!(f.submodule_line(&["status", "--ignore-submodules=none"]), BOTH);
    assert_eq!(
        f.submodule_line(&["status", "-uno", "--ignore-submodules=none"]),
        BOTH
    );
    assert_eq!(
        f.submodule_line(&[
            "-c",
            "submodule.sm.ignore=dirty",
            "status",
            "--ignore-submodules=none",
        ]),
        BOTH
    );
}

/// `set_diffopt_flags_from_submodule_config()` runs per path and after
/// everything else, so `submodule.<name>.ignore` beats both
/// `diff.ignoreSubmodules` and `-uno`.
#[test]
fn a_per_submodule_ignore_outranks_the_diff_wide_one_and_uno() {
    let f = Fixture::new("per-sm");
    assert_eq!(
        f.submodule_line(&[
            "-c",
            "diff.ignoreSubmodules=untracked",
            "-c",
            "submodule.sm.ignore=none",
            "status",
        ]),
        BOTH
    );
    assert_eq!(
        f.submodule_line(&[
            "-c",
            "diff.ignoreSubmodules=none",
            "-c",
            "submodule.sm.ignore=untracked",
            "status",
        ]),
        MODIFIED_ONLY
    );
    assert_eq!(
        f.submodule_line(&["-c", "submodule.sm.ignore=none", "status", "-uno"]),
        BOTH
    );
}

/// `-uno` only *adds* the untracked bit; it never clears what
/// `diff.ignoreSubmodules` put there, and an unset or `none` value leaves that
/// bit as the whole flag word.
#[test]
fn uno_layers_on_top_of_diff_ignore_submodules_rather_than_replacing_it() {
    let f = Fixture::new("diffcfg");
    assert_eq!(
        f.submodule_line(&["-c", "diff.ignoreSubmodules=none", "status", "-uno"]),
        MODIFIED_ONLY
    );
    // `dirty` hides the whole worktree side, untracked bit or not.
    assert_eq!(
        f.submodule_line(&["-c", "diff.ignoreSubmodules=dirty", "status", "-uno"]),
        ""
    );
    assert_eq!(f.submodule_line(&["-c", "diff.ignoreSubmodules=dirty", "status"]), "");
}

/// The same classification reaches porcelain v2's `<sub>` field, where the two
/// bits are the third and fourth characters of `S.MU`.
#[test]
fn the_porcelain_v2_sub_field_carries_the_same_two_bits() {
    let f = Fixture::new("v2");
    let sub = |args: &[&str]| -> String {
        let out = f.cmd_in(&f.work.clone(), args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find(|l| l.ends_with(" sm"))
            .and_then(|l| l.split(' ').nth(2).map(str::to_string))
            .unwrap_or_default()
    };
    assert_eq!(sub(&["status", "--porcelain=v2"]), "S.MU");
    assert_eq!(sub(&["status", "--porcelain=v2", "-uno"]), "S.M.");
    assert_eq!(
        sub(&["status", "--porcelain=v2", "--ignore-submodules=none"]),
        "S.MU"
    );
}
