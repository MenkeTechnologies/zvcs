//! `diff`, `diff-index` and `diff-files` reported a `-G` / `-S --pickaxe-regex`
//! pattern that does not compile with the `regex` crate's multi-line parse error.
//!
//! All three reach `diffcore_pickaxe()`, which compiles the needle with
//! `REG_EXTENDED | REG_NEWLINE` and hands a failure to `regcomp_or_die()`:
//! `regerror()` into a buffer, then `die("invalid regex: %s")`
//! (diffcore-pickaxe.c:219-228, 243-247). zvcs had two copies of the pickaxe
//! compiler; the one in `diff_pairs` (used by `log` and `diff-tree`) mapped the
//! syntax errors onto the `regerror()` wording through `line_log`'s shared
//! table, while the one in `diff_pickaxe` (used by `diff`, `diff-index`,
//! `diff-files` and `range-diff`) printed the crate's text. There is one
//! compiler now.
//!
//! Expectations measured from stock git 2.55.0 (macOS libc `regerror()`) under
//! the same environment.

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
    /// One commit of `f`, then a staged edit and a further unstaged one, so
    /// every one of the three verbs has a pair to run the pickaxe over.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-diff-pickaxe-regerror-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("f"), "alpha\nbeta\n").unwrap();
        f.run(&["add", "f"]);
        f.run(&["commit", "-q", "-m", "base"]);
        std::fs::write(f.work.join("f"), "alpha\nbeta\ngamma\n").unwrap();
        f.run(&["add", "f"]);
        std::fs::write(f.work.join("f"), "alpha\nbeta\ngamma\ndelta\n").unwrap();
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
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
            .env("TZ", "UTC")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

/// `regerror()`'s text is the C library's, since git compiles with the platform
/// `regcomp()`: Darwin's wording on macOS, glibc's on Linux (measured with glibc
/// 2.36's `regcomp(3)`/`regerror(3)`, which git 2.39 on the same system prints).
fn regerror(darwin: &'static str, glibc: &'static str) -> &'static str {
    if cfg!(all(target_os = "linux", target_env = "gnu")) { glibc } else { darwin }
}

#[test]
fn each_verb_dies_with_the_regerror_wording() {
    let f = Fixture::new("verbs");
    for (args, text) in [
        (&["diff", "HEAD", "-G["][..], regerror("brackets ([ ]) not balanced", "Invalid regular expression")),
        (&["diff", "--pickaxe-regex", "-S(a"][..], regerror("parentheses not balanced", "Unmatched ( or \\(")),
        (&["diff-index", "HEAD", "-Ga{1"][..], regerror("braces not balanced", "Unmatched \\{")),
        (&["diff-index", "--cached", "HEAD", "--pickaxe-regex", "-Sa\\"][..], regerror("trailing backslash (\\)", "Trailing backslash")),
        (&["diff-files", "-G[[:alpha:]"][..], regerror("brackets ([ ]) not balanced", "Unmatched [, [^, [:, [., or [=")),
    ] {
        let want = format!("fatal: invalid regex: {text}\n");
        assert_eq!(f.run(args), (String::new(), want, 128), "{args:?}");
    }
}

#[test]
fn a_valid_extended_pattern_still_selects_the_pair() {
    let f = Fixture::new("valid");
    // `(gam|del)ma?` is extended syntax: the grouping and `|` are operators.
    let (out, err, code) = f.run(&["diff", "HEAD", "--name-only", "-G(gam|del)ma?"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("f\n", "", 0));
    let (out, _, code) = f.run(&["diff-files", "--name-only", "-Gzeta|elt"]);
    assert_eq!((out.as_str(), code), ("f\n", 0));
}
