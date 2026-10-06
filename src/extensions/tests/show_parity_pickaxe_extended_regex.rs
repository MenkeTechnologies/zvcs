//! `git show -G<regex>` compiled its needle as a basic regular expression, and a
//! needle that did not compile died before any object was shown.
//!
//! `diffcore_pickaxe()` compiles `-G`, and `-S` under `--pickaxe-regex`, with
//! `REG_EXTENDED | REG_NEWLINE` (diffcore-pickaxe.c:242-246). zvcs built the
//! `-G` needle with the `--grep` compiler in its basic dialect, so `+`, `|`
//! and `(...)` were literals and `\(` opened a group.
//!
//! A needle that does not compile is `regcomp_or_die()`'s `die()`
//! (diffcore-pickaxe.c:219-228), reached from `diffcore_std()` only when a
//! commit is diffed. `cmd_show()` writes each object as it gets to it, so a
//! blob shown alone exits 0, and a blob named ahead of a commit is printed
//! before the fatal. zvcs died while parsing the options.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

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
    /// Three commits: `base`, `digits` (adds `beta 42`), `paren` (adds `(x)`).
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-show-pickaxe-ere-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        for (content, subject) in [
            ("alpha\n", "base"),
            ("alpha\nbeta 42\n", "digits"),
            ("alpha\nbeta 42\n(x)\n", "paren"),
        ] {
            std::fs::write(f.work.join("f"), content).unwrap();
            f.run(&["add", "f"]);
            f.run(&["commit", "-q", "-m", subject]);
        }
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

#[test]
fn the_needle_is_an_extended_expression() {
    let f = Fixture::new("ere");
    let ok = |s: &str| (s.to_string(), String::new(), 0);
    assert_eq!(f.run(&["show", "-s", "--format=%s", "-Gbeta [0-9]+", "HEAD~1", "HEAD"]), ok("digits\n"));
    assert_eq!(f.run(&["show", "-s", "--format=%s", "-Gzzz|beta", "HEAD~1", "HEAD"]), ok("digits\n"));
    assert_eq!(f.run(&["show", "-s", "--format=%s", "-G\\(x", "HEAD~1", "HEAD"]), ok("paren\n"));
}

/// `regerror()`'s text is the C library's, since git compiles with the platform
/// `regcomp()`: Darwin's wording on macOS, glibc's on Linux (measured with glibc
/// 2.36's `regcomp(3)`/`regerror(3)`, which git 2.39 on the same system prints).
fn regerror(darwin: &'static str, glibc: &'static str) -> &'static str {
    if cfg!(all(target_os = "linux", target_env = "gnu")) { glibc } else { darwin }
}

#[test]
fn a_bad_needle_dies_at_the_first_commit_diffed() {
    let f = Fixture::new("bad");
    let blob = "alpha\nbeta 42\n(x)\n";
    // A blob is never diffed, so nothing compiles the needle.
    assert_eq!(f.run(&["show", "-G(", "HEAD:f"]), (blob.to_string(), String::new(), 0));
    assert_eq!(
        f.run(&["show", "-s", "-G(", "HEAD"]),
        (String::new(), format!("fatal: invalid regex: {}\n", regerror("parentheses not balanced", "Unmatched ( or \\(")), 128)
    );
    // The blob ahead of the commit is already out when the commit dies.
    assert_eq!(
        f.run(&["show", "--pickaxe-regex", "-S[", "HEAD:f", "HEAD"]),
        (blob.to_string(), format!("fatal: invalid regex: {}\n", regerror("brackets ([ ]) not balanced", "Invalid regular expression")), 128)
    );
}
