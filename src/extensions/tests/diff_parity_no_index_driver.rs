//! `git diff --no-index` and the userdiff driver the operand names select.
//!
//! `cmd_diff()` sets the repository up before it notices the operands are not
//! tracked, so `o->repo->index` is live for the whole no-index comparison and
//! `diff_filespec_load_driver()` (diff.c:2308-2320) resolves each side's `diff`
//! attribute exactly as it would for a tracked path. Two things follow, and the
//! port used to have neither:
//!
//!   * `builtin_diff()` takes the hunk-heading pattern from the pre-image's
//!     driver and falls back to the post-image's (diff.c:4036-4038), so
//!     `diff.<name>.xfuncname` labels `@@` lines under `--no-index` too.
//!   * `init_diff_words_data()` takes the word regex the same way
//!     (diff.c:2346-2351) — the old side's driver, the new side's, then
//!     `diff.wordRegex` — so `--color-words` over a `diff=<lang>` path splits on
//!     that language's tokens rather than on whitespace runs.
//!
//! Outside any repository there is no attribute stack, so the same command over
//! the same files with the same `.gitattributes` beside them splits on
//! whitespace. That asymmetry is git's and is asserted here so a future "read
//! attributes from the cwd" shortcut cannot pass unnoticed.
//!
//! Every expectation was measured from stock git 2.55.0 over identical files.
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
    /// A repository holding two untracked files; nothing is ever added, so every
    /// comparison below really is a no-index one.
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("zvcs-diff-nidrv-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f
    }

    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.work.join(name), body).unwrap();
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

    /// A `--no-index` run, which exits 1 for a difference.
    fn diff(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().unwrap();
        assert_eq!(out.status.code(), Some(1), "`git {args:?}`: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

/// The eight-line bodies differ only in their last line, so `-U1` leaves exactly
/// one hunk whose heading has to come from somewhere.
const HEAD_OLD: &str = "#MARK one\n  a\n  b\n  c\n  d\n  e\n  f\n  g\n";
const HEAD_NEW: &str = "#MARK one\n  a\n  b\n  c\n  d\n  e\n  f\n  Z\n";

/// `diff.<name>.xfuncname` reaches a no-index pair. `def_ff` would reject
/// `#MARK one` outright — it only takes a line starting with an alphabetic
/// character or `_`/`$` — so the heading is proof the driver was consulted.
#[test]
fn no_index_hunk_heading_comes_from_the_attribute_driver() {
    let f = Fixture::new("func");
    f.write("q1", HEAD_OLD);
    f.write("q2", HEAD_NEW);
    f.write(".gitattributes", "* diff=mark\n");
    f.git(&["config", "diff.mark.xfuncname", "^#MARK.*$"]);

    let out = f.diff(&["diff", "--no-index", "-U1", "q1", "q2"]);
    let hunk = out.lines().find(|l| l.starts_with("@@")).expect("one hunk");
    assert_eq!(hunk, "@@ -7,2 +7,2 @@ #MARK one");

    // Without the attribute the same pair falls back to `def_ff`, which finds no
    // heading at all in this body.
    f.write(".gitattributes", "");
    let out = f.diff(&["diff", "--no-index", "-U1", "q1", "q2"]);
    let hunk = out.lines().find(|l| l.starts_with("@@")).expect("one hunk");
    assert_eq!(hunk, "@@ -7,2 +7,2 @@");
}

/// `--word-diff=plain` over a `diff=ada` pair. The built-in `ada` word regex
/// makes `+` and `-` words of their own, so the shared operators survive as
/// context between the changed identifiers; the whitespace-run default would
/// replace the whole line.
#[test]
fn no_index_word_regex_comes_from_the_attribute_driver() {
    let f = Fixture::new("word");
    f.write("pre", "a+b a-b\n");
    f.write("post", "x+y x-y\n");
    f.write(".gitattributes", "* diff=ada\n");

    let out = f.diff(&["diff", "--no-index", "--word-diff=plain", "pre", "post"]);
    let body = out.lines().find(|l| l.starts_with("[-") || l.starts_with("{+")).expect("word line");
    assert_eq!(body, "[-a-]{+x+}+[-b a-]{+y x+}-[-b-]{+y+}");

    // The same pair with no driver: one whitespace-delimited word per run.
    f.write(".gitattributes", "");
    let out = f.diff(&["diff", "--no-index", "--word-diff=plain", "pre", "post"]);
    let body = out.lines().find(|l| l.starts_with("[-") || l.starts_with("{+")).expect("word line");
    assert_eq!(body, "[-a+b a-b-]{+x+y x-y+}");
}

/// The command line wins outright: `init_diff_words_data()` only calls
/// `userdiff_word_regex()` when `o->word_regex` is still NULL (diff.c:2346), so
/// `--word-diff-regex=` is never overridden by the path's driver.
#[test]
fn an_explicit_word_regex_outranks_the_no_index_driver() {
    let f = Fixture::new("explicit");
    f.write("pre", "a+b a-b\n");
    f.write("post", "x+y x-y\n");
    f.write(".gitattributes", "* diff=ada\n");

    let out = f.diff(&[
        "diff",
        "--no-index",
        "--word-diff=plain",
        "--word-diff-regex=[^[:space:]]+",
        "pre",
        "post",
    ]);
    let body = out.lines().find(|l| l.starts_with("[-") || l.starts_with("{+")).expect("word line");
    assert_eq!(body, "[-a+b a-b-]{+x+y x-y+}");
}

/// `diff.wordRegex` is the last fallback, so a path whose driver carries a word
/// regex of its own is unaffected by it (diff.c:2350-2351).
#[test]
fn the_no_index_driver_outranks_diff_word_regex() {
    let f = Fixture::new("cfg");
    f.write("pre", "a+b a-b\n");
    f.write("post", "x+y x-y\n");
    f.write(".gitattributes", "* diff=ada\n");
    f.git(&["config", "diff.wordRegex", "[^[:space:]]+"]);

    let out = f.diff(&["diff", "--no-index", "--word-diff=plain", "pre", "post"]);
    let body = out.lines().find(|l| l.starts_with("[-") || l.starts_with("{+")).expect("word line");
    assert_eq!(body, "[-a-]{+x+}+[-b a-]{+y x+}-[-b-]{+y+}");

    // With the attribute gone, the config value is what splits the line — the
    // same spans the whitespace default would give, so the assertion below is
    // about the fallback firing at all, not about its shape.
    f.write(".gitattributes", "");
    let out = f.diff(&["diff", "--no-index", "--word-diff=plain", "pre", "post"]);
    let body = out.lines().find(|l| l.starts_with("[-") || l.starts_with("{+")).expect("word line");
    assert_eq!(body, "[-a+b a-b-]{+x+y x-y+}");
}

/// Started outside a repository there is no index and no attribute stack, so the
/// `.gitattributes` sitting next to the operands is an ordinary file.
#[test]
fn outside_a_repository_no_index_reads_no_attributes() {
    let root =
        std::env::temp_dir().join(format!("zvcs-diff-nidrv-bare-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("pre"), "a+b a-b\n").unwrap();
    std::fs::write(root.join("post"), "x+y x-y\n").unwrap();
    std::fs::write(root.join(".gitattributes"), "* diff=ada\n").unwrap();

    let out = Command::new(BIN)
        .args(["diff", "--no-index", "--word-diff=plain", "pre", "post"])
        .current_dir(&root)
        .env("HOME", &root)
        .env("ZVCS_HOME", root.join("zvcs"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CEILING_DIRECTORIES", &root)
        .env("LC_ALL", "C")
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let body = stdout
        .lines()
        .find(|l| l.starts_with("[-") || l.starts_with("{+"))
        .expect("word line");
    assert_eq!(body, "[-a+b a-b-]{+x+y x-y+}");
}
