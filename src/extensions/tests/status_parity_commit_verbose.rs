//! `git commit -v`'s patch in `COMMIT_EDITMSG`: which tree it is measured
//! against, how many times `-v` may be given, and what `commit.verbose` means.
//!
//! git renders that patch from the very same `wt_status_print()` that writes the
//! commented status block (`run_status(s->fp, …)`, builtin/commit.c:1025), so
//! three things follow that an appended `git diff --cached` does not reproduce:
//!
//!   * `opt.def = s->is_initial ? empty_tree_oid_hex(…) : s->reference`
//!     (wt-status.c:1173). Under `--amend` the reference is `HEAD^1`
//!     (builtin/commit.c:573), and amending a *root* commit leaves `HEAD`
//!     resolvable while `HEAD^1` is not — so the patch is the whole tree
//!     against the empty tree, not the empty difference from `HEAD`.
//!   * `if (s->fp != stdout) { use_color = GIT_COLOR_NEVER;
//!     wt_status_add_cut_line(s); }` (wt-status.c:1189-1194) puts the scissors
//!     line above the patch — once, since `--cleanup=scissors` already wrote one.
//!   * `OPT__VERBOSE` is `OPT_COUNTUP`, so a second `-v` labels the staged patch
//!     with `c/`…`i/` prefixes and appends the unstaged one with `i/`…`w/`
//!     (wt-status.c:1195-1213). `commit.verbose` is read with
//!     `git_config_bool_or_int()` and a negative value floors to zero
//!     (builtin/commit.c:1827-1828), rather than being truthy.
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
    /// The editor script: it copies the buffer it is handed to `capture` and
    /// leaves it untouched, so the commit aborts on an unchanged template and
    /// the captured file is exactly what git wrote.
    capture: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// One commit on `main` holding `file`, which is what an `--amend` of a root
    /// commit needs.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-st-verbose-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let capture = root.join("captured");
        let editor = root.join("editor.sh");
        std::fs::write(
            &editor,
            format!("#!/bin/sh\ncp \"$1\" {}\nexit 0\n", capture.display()),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&editor, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let f = Fixture { root, work, capture };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "content\n").unwrap();
        f.git(&["add", "file"]);
        f.git(&["commit", "-q", "-m", "one"]);
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
            .env("GIT_EDITOR", self.root.join("editor.sh"))
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

    /// Run `git commit <args>` through the capturing editor and hand back the
    /// template it was given.
    fn template(&self, args: &[&str]) -> String {
        self.template_with(&[], args)
    }

    /// The same, with `-c <key>=<value>` settings ahead of the subcommand.
    fn template_with(&self, config: &[&str], args: &[&str]) -> String {
        let _ = std::fs::remove_file(&self.capture);
        let mut full: Vec<&str> = Vec::new();
        for c in config {
            full.extend_from_slice(&["-c", c]);
        }
        full.push("commit");
        full.extend_from_slice(args);
        let out = self.cmd(&full).output().unwrap();
        assert!(
            out.status.success(),
            "`git {full:?}` failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::fs::read_to_string(&self.capture).expect("editor never ran")
    }
}

/// The cut line `wt_status_add_cut_line()` writes, as it reaches the template.
const CUT: &str = "# ------------------------ >8 ------------------------\n\
                   # Do not modify or remove the line above.\n\
                   # Everything below it will be ignored.\n";

/// Amending a root commit: `HEAD^1` does not resolve, so the patch is the index
/// against the *empty tree* even though `HEAD` is right there. Measuring against
/// `HEAD` (what a bare `git diff --cached` would do) yields no patch at all,
/// because an amend that changes nothing has an index identical to `HEAD`.
#[test]
fn amending_a_root_commit_diffs_against_the_empty_tree() {
    let f = Fixture::new("root");
    let t = f.template(&["--amend", "-v"]);
    let (_, below) = t.split_once(CUT).expect("no cut line above the patch");
    let (index_line, rest) = below
        .strip_prefix("diff --git a/file b/file\nnew file mode 100644\n")
        .and_then(|r| r.split_once('\n'))
        .unwrap_or_else(|| panic!("template was:\n{t}"));
    // The abbreviation length is the repository's, so only the shape is pinned.
    assert!(index_line.starts_with("index 0000000"), "{index_line}");
    assert_eq!(
        rest,
        "--- /dev/null\n+++ b/file\n@@ -0,0 +1 @@\n+content\n",
        "template was:\n{t}"
    );
}

/// A non-initial `--amend` measures against `HEAD^1`, so the patch shows the
/// commit being replaced rather than the empty difference from `HEAD`.
#[test]
fn amending_a_child_commit_diffs_against_the_parent() {
    let f = Fixture::new("parent");
    std::fs::write(f.work.join("file"), "content\nmore\n").unwrap();
    f.git(&["add", "file"]);
    f.git(&["commit", "-q", "-m", "two"]);
    let t = f.template(&["--amend", "-v"]);
    let (_, below) = t.split_once(CUT).expect("no cut line above the patch");
    assert!(
        below.starts_with("diff --git a/file b/file\n") && below.contains("\n+more\n"),
        "patch was measured against HEAD, not HEAD^1:\n{t}"
    );
}

/// `-v -v` labels the staged patch and appends the unstaged one, with the two
/// prefix pairs git substitutes. The label is preceded by one empty commented
/// line — `wt_longstatus_print_trailer()`, which only the editor block prints.
#[test]
fn a_second_v_labels_the_staged_patch_and_appends_the_unstaged_one() {
    let f = Fixture::new("twice");
    std::fs::write(f.work.join("file"), "content\nmore\n").unwrap();
    f.git(&["add", "file"]);
    std::fs::write(f.work.join("file"), "content\nmore\ndirty\n").unwrap();
    let t = f.template(&["--amend", "-v", "-v"]);
    let (_, below) = t.split_once(CUT).expect("no cut line above the patch");
    let want_head = "#\n# Changes to be committed:\ndiff --git c/file i/file\n";
    assert!(below.starts_with(want_head), "staged half was:\n{below}");
    assert!(
        below.contains(&format!(
            "# {}\n# Changes not staged for commit:\ndiff --git i/file w/file\n",
            "-".repeat(50)
        )),
        "unstaged half missing:\n{below}"
    );
}

/// One `-v` leaves the patch unlabelled and on its configured prefixes, and
/// writes exactly one cut line.
#[test]
fn one_v_writes_a_single_unlabelled_patch() {
    let f = Fixture::new("once");
    let t = f.template(&["--amend", "-v"]);
    assert_eq!(t.matches(CUT).count(), 1, "cut line not written once:\n{t}");
    assert_eq!(t.matches("diff --git ").count(), 1, "more than one patch:\n{t}");
    assert!(!t.contains("diff --git c/"), "unlabelled patch took the -vv prefixes:\n{t}");
}

/// `--cleanup=scissors` writes the cut line itself, above the status block;
/// `s->added_cut_line` then keeps `wt_longstatus_print_verbose()` from writing a
/// second one, so the message is still truncated at the only one there is.
#[test]
fn scissors_cleanup_and_verbose_share_one_cut_line() {
    let f = Fixture::new("scissors");
    let t = f.template(&["--amend", "-v", "--cleanup=scissors"]);
    assert_eq!(t.matches(CUT).count(), 1, "two cut lines:\n{t}");
    let (above, _) = t.split_once(CUT).unwrap();
    assert!(
        !above.contains("# On branch"),
        "scissors line belongs above the status block:\n{t}"
    );
}

/// Without `-v` there is no patch and no cut line at all.
#[test]
fn no_verbose_writes_no_patch_and_no_cut_line() {
    let f = Fixture::new("plain");
    let t = f.template(&["--amend"]);
    assert!(!t.contains("diff --git "), "unasked-for patch:\n{t}");
    assert!(!t.contains(CUT), "unasked-for cut line:\n{t}");
}

/// `commit.verbose` speaks only for an unspecified `-v`, is read as a bool *or*
/// an int, and floors negatives to zero rather than treating them as truthy.
/// `--no-verbose` resets the count whatever the config says.
#[test]
fn commit_verbose_config_is_a_bool_or_int_floored_at_zero() {
    let f = Fixture::new("config");
    std::fs::write(f.work.join("file"), "content\nmore\n").unwrap();
    f.git(&["add", "file"]);
    std::fs::write(f.work.join("file"), "content\nmore\ndirty\n").unwrap();
    let patches = |value: &str, extra: &[&str]| -> usize {
        let setting = format!("commit.verbose={value}");
        let mut args = vec!["--amend"];
        args.extend_from_slice(extra);
        f.template_with(&[&setting], &args).matches("diff --git ").count()
    };
    assert_eq!(patches("true", &[]), 1, "commit.verbose=true");
    assert_eq!(patches("1", &[]), 1, "commit.verbose=1");
    assert_eq!(patches("2", &[]), 2, "commit.verbose=2");
    assert_eq!(patches("false", &[]), 0, "commit.verbose=false");
    assert_eq!(patches("-2", &[]), 0, "commit.verbose=-2 must floor to 0");
    assert_eq!(patches("0", &[]), 0, "commit.verbose=0");
    // The command line wins outright, in both directions.
    assert_eq!(patches("false", &["-v"]), 1, "-v over commit.verbose=false");
    assert_eq!(patches("false", &["-v", "-v"]), 2, "-v -v over commit.verbose=false");
    assert_eq!(patches("3", &["--no-verbose"]), 0, "--no-verbose over commit.verbose=3");
}
