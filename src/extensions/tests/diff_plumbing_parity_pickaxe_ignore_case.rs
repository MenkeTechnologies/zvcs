//! `-i` / `--regexp-ignore-case` in the diff plumbing and `show`.
//!
//! The flag is not one of `add_diff_options()`'s: `handle_revision_opt()` sets
//! `DIFF_PICKAXE_IGNORE_CASE` from it (revision.c:2690-2692), and
//! `diffcore_pickaxe()` then folds case for `-S` and `-G` alike
//! (diffcore-pickaxe.c:241-272). Every command that runs `setup_revisions()` takes
//! it — `diff-index`, `diff-files`, `diff-tree` and `show` refused it — while
//! `diff-pairs`, which parses only the diff option table, still calls it an unknown
//! switch. The grep dialect flags handled beside it (revision.c:2686-2696) only
//! choose `grep_filter.pattern_type_option`, so they are accepted and inert.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
use std::process::{Command, Stdio};

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
    /// `f` goes from `Foo` to `foo bar` + `FOO` in HEAD and gains `FoO` in the
    /// worktree; `g` gains a staged `foo`. Only a case-folding count sees `f` change.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-plumbing-pickaxe-icase-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("f"), "a\nb\nFoo\n").unwrap();
        std::fs::write(f.work.join("g"), "q\n").unwrap();
        f.run(&["add", "f", "g"]);
        f.run(&["commit", "-q", "-m", "one"]);
        std::fs::write(f.work.join("f"), "a\nfoo bar\nFOO\n").unwrap();
        f.run(&["commit", "-q", "-am", "two"]);
        std::fs::write(f.work.join("f"), "a\nfoo bar\nFOO\nx\nFoO\n").unwrap();
        std::fs::write(f.work.join("g"), "q\nfoo\n").unwrap();
        f.run(&["add", "g"]);
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
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC");
        c
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = self.cmd(args).output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn ok(&self, args: &[&str]) -> String {
        let (out, err, code) = self.run(args);
        assert_eq!((err.as_str(), code), ("", 0), "{args:?}");
        out
    }
}

#[test]
fn diff_files_folds_case_for_s_and_g() {
    let f = Fixture::new("files");
    // One `foo` on each side case-sensitively; two against three folded.
    assert_eq!(f.ok(&["diff-files", "-Sfoo", "--name-status"]), "");
    assert_eq!(f.ok(&["diff-files", "-i", "-Sfoo", "--name-status"]), "M\tf\n");
    assert_eq!(f.ok(&["diff-files", "--regexp-ignore-case", "-G^fOo$", "--name-only"]), "f\n");
}

#[test]
fn diff_index_folds_case() {
    let f = Fixture::new("index");
    assert_eq!(f.ok(&["diff-index", "-SFOO", "--name-only", "HEAD"]), "");
    assert_eq!(f.ok(&["diff-index", "-i", "-SFOO", "--name-only", "HEAD"]), "f\ng\n");
}

/// `regerror()`'s text is the C library's, since git compiles with the platform
/// `regcomp()`: Darwin's wording on macOS, glibc's on Linux (measured with glibc
/// 2.36's `regcomp(3)`/`regerror(3)`, which git 2.39 on the same system prints).
fn regerror(darwin: &'static str, glibc: &'static str) -> &'static str {
    if cfg!(all(target_os = "linux", target_env = "gnu")) { glibc } else { darwin }
}

#[test]
fn diff_tree_folds_case_and_still_reports_a_bad_regex() {
    let f = Fixture::new("tree");
    assert_eq!(f.ok(&["diff-tree", "-r", "--name-only", "-GfOo", "HEAD~", "HEAD"]), "");
    assert_eq!(f.ok(&["diff-tree", "-r", "--name-only", "-i", "-GfOo", "HEAD~", "HEAD"]), "f\n");
    assert_eq!(
        f.ok(&["diff-tree", "-r", "-p", "--regexp-ignore-case", "-SfOO", "HEAD~", "HEAD"]),
        "diff --git a/f b/f\nindex 4074946..b00a7db 100644\n--- a/f\n+++ b/f\n\
         @@ -1,3 +1,3 @@\n a\n-b\n-Foo\n+foo bar\n+FOO\n"
    );
    let (out, err, code) = f.run(&["diff-tree", "-r", "-i", "-G(", "HEAD~", "HEAD"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", format!("fatal: invalid regex: {}\n", regerror("parentheses not balanced", "Unmatched ( or \\(")).as_str(), 128)
    );
}

#[test]
fn show_folds_case() {
    let f = Fixture::new("show");
    assert_eq!(f.ok(&["show", "-i", "-GfOo", "--format=%s", "--name-only"]), "two\n\nf\n");
    assert_eq!(
        f.ok(&["show", "--regexp-ignore-case", "-Sfoo", "--format=%s", "--stat"]),
        "two\n\n f | 4 ++--\n 1 file changed, 2 insertions(+), 2 deletions(-)\n"
    );
}

#[test]
fn diff_pairs_still_calls_it_an_unknown_switch() {
    let f = Fixture::new("pairs");
    let raw = f.cmd(&["diff-tree", "-r", "-z", "HEAD~", "HEAD"]).output().unwrap().stdout;
    let mut child = f
        .cmd(&["diff-pairs", "-z", "-i", "-Sfoo"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(&raw).unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(129));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("error: unknown switch `i'\n"));
}

#[test]
fn the_grep_dialect_flags_are_accepted_and_inert() {
    let f = Fixture::new("dialect");
    assert_eq!(f.ok(&["diff-files", "-F", "--name-only"]), "f\n");
    assert_eq!(f.ok(&["diff-index", "-E", "--name-only", "HEAD"]), "f\ng\n");
    assert_eq!(f.ok(&["diff-tree", "-P", "--name-only", "HEAD~", "HEAD"]), "f\n");
    assert_eq!(f.ok(&["show", "--basic-regexp", "--format=%s", "--name-only"]), "two\n\nf\n");
}
