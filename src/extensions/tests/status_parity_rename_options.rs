//! `git status`'s two rename variables, `no_renames` and `rename_score_arg`, and
//! the fact that `cmd_status()` resolves them *after* parsing rather than in
//! command-line order (builtin/commit.c:1646-1653):
//!
//! ```c
//! if (no_renames != -1)
//!         s.detect_rename = !no_renames;
//! if ((intptr_t)rename_score_arg != -1) {
//!         if (s.detect_rename < DIFF_DETECT_RENAME)
//!                 s.detect_rename = DIFF_DETECT_RENAME;
//!         if (rename_score_arg)
//!                 s.rename_score = parse_rename_score(&rename_score_arg);
//! }
//! ```
//!
//! Three behaviours fall out of that shape, none of which a left-to-right reading
//! of the command line predicts, and all three were measured from stock git
//! 2.55.0 in identical throwaway repositories under the same pinned environment:
//!
//! * `-M` never *lowers* detection (`s.detect_rename < DIFF_DETECT_RENAME`), so
//!   it leaves `status.renames = copies` at `DIFF_DETECT_COPY` instead of
//!   demoting it to plain renames.
//! * the `-M` clause runs last, so `--find-renames=<n> --no-renames` still
//!   detects renames.
//! * `opt_parse_rename_score()` (builtin/commit.c:185-196) only stores the raw
//!   string, and `parse_rename_score()` (diff.c:6344-6378) stops at the first
//!   byte it cannot consume without anyone checking the remainder — so
//!   `--find-renames=bogus` is a similarity of 0, not a usage error.
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

/// Ten identical lines, long enough that a copy scores 100% and a rename is
/// exact, so every assertion below turns on the *option* rather than on the
/// similarity arithmetic.
const BODY: &str = "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\nline 8\nline 9\nline 10\n";

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-st-ren-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("src"), BODY).unwrap();
        f.git(&["add", "src"]);
        f.git(&["commit", "-q", "-m", "one"]);
        f
    }

    /// `src` renamed to `dst`, staged: one deletion and one addition for
    /// `diffcore_rename()` to pair up.
    fn staged_rename(tag: &str) -> Self {
        let f = Fixture::new(tag);
        f.git(&["mv", "src", "dst"]);
        f
    }

    /// `src` kept *and* copied to `dst`, with `src` also grown by a line so the
    /// pair is a copy rather than a rename — only `DIFF_DETECT_COPY` reports it.
    fn staged_copy(tag: &str) -> Self {
        let f = Fixture::new(tag);
        std::fs::write(f.work.join("dst"), BODY).unwrap();
        std::fs::write(f.work.join("src"), format!("{BODY}extra\n")).unwrap();
        f.git(&["add", "-A"]);
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

    /// stdout, stderr and the exit status, so a test can pin a *non*-failure as
    /// firmly as it pins the report.
    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = self.cmd(args).output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().unwrap_or(-1),
        )
    }
}

/// `parse_rename_score()` consumes `[0-9.]*%?` and stops; `cmd_status()` never
/// looks at what is left, so a trailing `x` is not an error and the score it
/// yields is the part that parsed (`50x` → 50%, which still pairs an exact
/// rename).
#[test]
fn a_similarity_with_a_trailing_remainder_is_not_a_usage_error() {
    let f = Fixture::staged_rename("remainder");
    for arg in ["--find-renames=50x", "-M50x"] {
        assert_eq!(
            f.run(&["status", "-s", arg]),
            ("R  src -> dst\n".to_string(), String::new(), 0),
            "{arg}"
        );
    }
}

/// The whole argument unparseable is `num = 0`, `scale = 1`, i.e. a similarity of
/// 0 — every deletion/addition pair matches, and still no error.
#[test]
fn an_unparseable_similarity_is_a_score_of_zero_rather_than_a_rejection() {
    let f = Fixture::staged_rename("bogus");
    assert_eq!(
        f.run(&["status", "-s", "--find-renames=bogus"]),
        ("R  src -> dst\n".to_string(), String::new(), 0)
    );
    // `-Mbogus`: the same string arriving through the short-option cluster.
    assert_eq!(
        f.run(&["status", "-s", "-Mbogus"]),
        ("R  src -> dst\n".to_string(), String::new(), 0)
    );
}

/// `--find-renames` is applied after `--no-renames` whatever the order on the
/// command line, so it wins from either side.
#[test]
fn find_renames_outranks_no_renames_in_both_orders() {
    let f = Fixture::staged_rename("order");
    for args in [
        ["--find-renames=90", "--no-renames"],
        ["--no-renames", "--find-renames=90"],
    ] {
        assert_eq!(
            f.run(&["status", "-s", args[0], args[1]]),
            ("R  src -> dst\n".to_string(), String::new(), 0),
            "{args:?}"
        );
    }
}

/// `--no-renames` alone still turns detection off — the clause above only fires
/// when `-M` was given.
#[test]
fn no_renames_alone_still_reports_the_pair_as_a_delete_and_an_add() {
    let f = Fixture::staged_rename("off");
    assert_eq!(
        f.run(&["status", "-s", "--no-renames"]),
        ("A  dst\nD  src\n".to_string(), String::new(), 0)
    );
}

/// `if (s.detect_rename < DIFF_DETECT_RENAME)`: `DIFF_DETECT_COPY` is 2, so `-M`
/// leaves a configured `copies` alone rather than writing 1 over it.
#[test]
fn find_renames_does_not_demote_configured_copy_detection() {
    let f = Fixture::staged_copy("copies");
    for key in ["status.renames=copies", "diff.renames=copies"] {
        for flag in ["-M", "--find-renames=90"] {
            assert_eq!(
                f.run(&["-c", key, "status", "-s", flag]),
                ("C  src -> dst\nM  src\n".to_string(), String::new(), 0),
                "{key} {flag}"
            );
        }
    }
}

/// `--renames` is `no_renames = 0`, i.e. `s.detect_rename = 1` — a plain
/// assignment, so it *does* demote `copies`. The contrast with the test above is
/// the whole point of the two clauses being separate.
#[test]
fn the_renames_flag_does_overwrite_configured_copy_detection() {
    let f = Fixture::staged_copy("renames-flag");
    assert_eq!(
        f.run(&["-c", "status.renames=copies", "status", "-s", "--renames"]),
        ("A  dst\nM  src\n".to_string(), String::new(), 0)
    );
}

/// A falsy `status.renames` is still only a *default*: `-M` raises it back to
/// `DIFF_DETECT_RENAME`, which is what the `<` comparison is there for.
#[test]
fn find_renames_raises_detection_that_config_had_turned_off() {
    let f = Fixture::staged_rename("off-then-on");
    assert_eq!(
        f.run(&["-c", "status.renames=false", "status", "-s", "-M"]),
        ("R  src -> dst\n".to_string(), String::new(), 0)
    );
}
