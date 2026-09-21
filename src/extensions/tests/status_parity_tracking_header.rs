//! The two tracking reports: `git status -sb`'s `## <branch>...<upstream>` line
//! and the long format's `Your branch …` block, for a branch whose upstream
//! lives in this very repository and for the reports `git commit` prints.
//!
//! `wt_shortstatus_print_tracking()` goes through `branch_get(branch_name)` and
//! `stat_tracking_info(branch, &num_ours, &num_theirs, &base, 0, …)`
//! (wt-status.c:2124-2130), which for `branch.<name>.remote = .` resolves
//! `branch.<name>.merge` itself — `set_merge()` in remote.c consults no fetch
//! refspec for a local remote, so there is no remote-tracking ref to look up.
//!
//! `wt_longstatus_print_tracking()` passes `!s->commit_template` as
//! `format_tracking_info()`'s divergence-advice flag (wt-status.c:1231-1232), and
//! `cmd_commit()` sets `s->commit_template = 1` for every report it prints
//! (builtin/commit.c:1809) — `--dry-run` and the report that stands in for a
//! refusal included, not only the `COMMIT_EDITMSG` block.
//!
//! Every expectation was measured from stock git 2.55.0 in an identical
//! throwaway repository under the same pinned environment.
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
    /// `main` and `upstream` off one root commit, `upstream` one commit ahead,
    /// and `main` tracking it through `branch.main.remote = .`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-st-track-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "one\n").unwrap();
        f.git(&["add", "file"]);
        f.git(&["commit", "-q", "-m", "one"]);
        f.git(&["checkout", "-q", "-b", "upstream"]);
        std::fs::write(f.work.join("file"), "two\n").unwrap();
        f.git(&["commit", "-q", "-a", "-m", "two"]);
        f.git(&["checkout", "-q", "main"]);
        f.git(&["branch", "--set-upstream-to=upstream"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
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

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn stdout(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

/// A `.`-remote upstream reaches the short header, counts and all.
#[test]
fn the_short_header_names_an_upstream_in_this_repository() {
    let f = Fixture::new("local");
    assert_eq!(
        f.stdout(&["status", "-s", "-b"]),
        "## main...upstream [behind 1]\n"
    );
}

/// Both directions of divergence, in git's `[ahead N, behind M]` order.
#[test]
fn the_short_header_counts_both_directions() {
    let f = Fixture::new("diverged");
    std::fs::write(f.work.join("other"), "mine\n").unwrap();
    f.git(&["add", "other"]);
    f.git(&["commit", "-q", "-m", "mine"]);
    assert_eq!(
        f.stdout(&["status", "-s", "-b"]),
        "## main...upstream [ahead 1, behind 1]\n"
    );
    // `-z` prints the same header, NUL-terminated.
    let z = f.stdout(&["status", "-s", "-z", "-b"]);
    assert!(
        z.starts_with("## main...upstream [ahead 1, behind 1]\0"),
        "{z:?}"
    );
}

/// A deleted upstream is `[gone]`, not a missing half-line.
#[test]
fn a_deleted_upstream_is_gone() {
    let f = Fixture::new("gone");
    f.git(&["branch", "-D", "upstream"]);
    assert_eq!(f.stdout(&["status", "-s", "-b"]), "## main...upstream [gone]\n");
}

/// `--no-ahead-behind` collapses the counts to `[different]`.
#[test]
fn quick_ahead_behind_collapses_to_different() {
    let f = Fixture::new("quick");
    assert_eq!(
        f.stdout(&["status", "-s", "-b", "--no-ahead-behind"]),
        "## main...upstream [different]\n"
    );
}

/// `git status`'s long format keeps the divergence advice; every report `git
/// commit` prints drops it, because they all carry `s->commit_template`.
#[test]
fn only_status_itself_prints_the_divergence_advice() {
    let f = Fixture::new("advice");
    std::fs::write(f.work.join("other"), "mine\n").unwrap();
    f.git(&["add", "other"]);
    f.git(&["commit", "-q", "-m", "mine"]);
    std::fs::write(f.work.join("file"), "staged\n").unwrap();
    f.git(&["add", "file"]);

    let hint = "  (use \"git pull\" if you want to integrate the remote branch with yours)";
    let status = f.stdout(&["status"]);
    assert!(status.contains("have diverged"), "{status}");
    assert!(status.contains(hint), "status lost the advice:\n{status}");

    let dry_run = f.stdout(&["commit", "--dry-run"]);
    assert!(dry_run.contains("have diverged"), "{dry_run}");
    assert!(!dry_run.contains(hint), "--dry-run kept the advice:\n{dry_run}");
}
