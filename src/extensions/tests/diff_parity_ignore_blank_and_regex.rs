//! `--ignore-blank-lines` and `-I<regex>` together.
//!
//! xdiff runs the two markers in order and the second one refuses to undo the
//! first:
//!
//! ```c
//! if (xpp->flags & XDF_IGNORE_BLANK_LINES)
//!         xdl_mark_ignorable_lines(xscr, &xe, xpp->flags);
//!
//! if (xpp->ignore_regex)
//!         xdl_mark_ignorable_regex(xscr, &xe, xpp);
//! ```
//! (xdiff/xdiffi.c:1106-1110), and `xdl_mark_ignorable_regex()` opens with
//!
//! ```c
//! /*
//!  * Do not override --ignore-blank-lines.
//!  */
//! if (xch->ignore)
//!         continue;
//! ```
//! (xdiff/xdiffi.c:1070-1074).
//!
//! So the two verdicts are an **or**: a change every record of which is blank
//! stays ignored even when `-I` is in force and none of its patterns match that
//! change. The port read the second pass as last-writer-wins, which made `-I`
//! silently cancel `--ignore-blank-lines` — every blank-only change came back
//! into the patch as soon as a single `-I` was given.
//!
//! The fixture puts the two ignorable changes far enough apart to be separate
//! hunks: line 1 is regex-ignorable on both sides, and the only other change is
//! a blank line appended at the end. Each option alone leaves exactly the other
//! change; together they leave nothing.
//!
//! Every expectation was measured from stock git 2.55.0 over the same fixture.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

const OLD: &str = "IGNORE_ME_old\nc2\nc3\nc4\nc5\nc6\nc7\nc8\nc9\nc10\nc11\nc12\n";
const NEW: &str = "IGNORE_ME_new\nc2\nc3\nc4\nc5\nc6\nc7\nc8\nc9\nc10\nc11\nc12\n\n";

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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-diff-ibl-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("f"), OLD).unwrap();
        f.git(&["add", "f"]);
        f.git(&["commit", "-q", "-m", "base"]);
        std::fs::write(f.work.join("f"), NEW).unwrap();
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

    /// The `@@` lines of a patch, which is the part these options decide.
    fn hunks(&self, args: &[&str]) -> Vec<String> {
        self.stdout(args)
            .lines()
            .filter(|l| l.starts_with("@@"))
            .map(str::to_owned)
            .collect()
    }
}

/// The two options alone: each leaves exactly the change the other suppresses.
#[test]
fn each_ignore_option_alone_leaves_the_other_change() {
    let f = Fixture::new("alone");
    assert_eq!(f.hunks(&["diff", "-I^IGNORE_ME", "--", "f"]), ["@@ -10,3 +10,4 @@ c9"]);
    assert_eq!(f.hunks(&["diff", "--ignore-blank-lines", "--", "f"]), ["@@ -1,4 +1,4 @@"]);
}

/// Together they leave nothing at all: the regex pass never revives the
/// blank-only change the blank-line pass already marked.
#[test]
fn a_regex_never_revives_a_blank_only_change() {
    let f = Fixture::new("both");
    let both = ["--ignore-blank-lines", "-I^IGNORE_ME"];
    for verb in [
        vec!["diff"],
        vec!["diff", "HEAD"],
        vec!["diff-files", "-p"],
        vec!["diff-index", "-p", "HEAD"],
    ] {
        let mut args = verb.clone();
        args.extend_from_slice(&both);
        args.extend_from_slice(&["--", "f"]);
        assert_eq!(f.stdout(&args), "", "{verb:?}");
    }
}

/// The same question for the summary formats, which count the very same change
/// script: an all-ignored pair contributes no row.
#[test]
fn an_all_ignored_pair_is_absent_from_the_summary_formats() {
    let f = Fixture::new("stat");
    let both = ["--ignore-blank-lines", "-I^IGNORE_ME"];
    for fmt in ["--stat", "--numstat", "--shortstat", "--raw", "--name-only"] {
        let mut args = vec!["diff", fmt];
        args.extend_from_slice(&both);
        args.extend_from_slice(&["--", "f"]);
        assert_eq!(f.stdout(&args), "", "{fmt}");
    }
}

/// `--exit-code` reads `o->found_changes`, which an all-ignored pair leaves
/// clear, so the run reports "no difference" — the cheapest way to see that the
/// ignore verdict reached the decision and not just the renderer.
#[test]
fn an_all_ignored_pair_reports_no_difference() {
    let f = Fixture::new("exit");
    let out = f
        .cmd(&["diff", "--exit-code", "--ignore-blank-lines", "-I^IGNORE_ME", "--", "f"])
        .output()
        .unwrap();
    assert_eq!((out.stdout.as_slice(), out.status.code()), (b"".as_slice(), Some(0)), "{out:?}");

    // With only the regex the blank-line change survives, so the same run is a
    // difference.
    let out = f.cmd(&["diff", "--exit-code", "-I^IGNORE_ME", "--", "f"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1), "{out:?}");
}
