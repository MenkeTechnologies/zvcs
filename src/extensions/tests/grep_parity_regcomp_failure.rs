//! `git grep` with a pattern `regcomp()` refuses.
//!
//! `compile_grep_patterns()` compiles every atom in list order and a failure
//! dies through `compile_regexp_failed()`:
//!
//! ```c
//! if (p->no)
//!         xsnprintf(where, sizeof(where), "In '%s' at %d, ", p->origin, p->no);
//! else if (p->origin)
//!         xsnprintf(where, sizeof(where), "%s, ", p->origin);
//! ...
//! die("%s'%s': %s", where, p->pattern, error);
//! ```
//! (`grep.c:220-233`, v2.56.0)
//!
//! The origin is `-e option`, `command line` for the bare pattern, or the `-f`
//! file with the pattern's number among its non-empty lines. The port accepted
//! the unterminated interval `a\{1` (its engine read the brace literally) and
//! reported the rest in the regex crate's words.
//!
//! The `regerror()` tail is the C library's: the one stock git 2.56.0 prints on
//! macOS, and glibc's on Linux — the same text `--grep` and `-L` use.
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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-grep-regcomp-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("f"), "a{1\nok\n").unwrap();
        f.run(&["add", "f"]);
        std::fs::write(f.root.join("pats"), "ok\n\nx\\(\n").unwrap();
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
            .env("LC_ALL", "C")
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
fn regcomp_failures_name_origin_pattern_and_reason() {
    let f = Fixture::new("fail");
    let pats = f.root.join("pats");
    let pats = pats.to_str().unwrap();
    for (args, msg) in [
        (&["grep", "a\\{1"][..], format!("fatal: command line, 'a\\{{1': {}\n", regerror("braces not balanced", "Unmatched \\{"))),
        (&["grep", "-e", "a\\{1,"][..], format!("fatal: -e option, 'a\\{{1,': {}\n", regerror("braces not balanced", "Unmatched \\{"))),
        (&["grep", "-E", "-e", "a{1"][..], format!("fatal: -e option, 'a{{1': {}\n", regerror("braces not balanced", "Unmatched \\{"))),
        (&["grep", "-e", "ok", "-e", "["][..], format!("fatal: -e option, '[': {}\n", regerror("brackets ([ ]) not balanced", "Invalid regular expression"))),
        (&["grep", "-e", "\\("][..], format!("fatal: -e option, '\\(': {}\n", regerror("parentheses not balanced", "Unmatched ( or \\("))),
        (&["grep", "-e", "a\\{2,1\\}"][..], format!("fatal: -e option, 'a\\{{2,1\\}}': {}\n", regerror("invalid repetition count(s)", "Invalid content of \\{\\}"))),
        // A back reference may only name a group that has closed.
        (&["grep", "-e", "\\(o\\)\\2"][..], format!("fatal: -e option, '\\(o\\)\\2': {}\n", regerror("invalid backreference number", "Invalid back reference"))),
        (&["grep", "-e", "\\(o\\1\\)"][..], format!("fatal: -e option, '\\(o\\1\\)': {}\n", regerror("invalid backreference number", "Invalid back reference"))),
        (&["grep", "-f", pats][..], format!("fatal: In '{pats}' at 2, 'x\\(': {}\n", regerror("parentheses not balanced", "Unmatched ( or \\("))),
    ] {
        assert_eq!(f.run(args), (String::new(), msg, 128), "{args:?}");
    }
}

/// `-F` never reaches `regcomp()`, and a well-formed interval still compiles.
#[test]
fn literal_and_valid_patterns_still_search() {
    let f = Fixture::new("ok");
    assert_eq!(f.run(&["grep", "-F", "a{1"]), ("f:a{1\n".into(), String::new(), 0));
    assert_eq!(f.run(&["grep", "-E", "-e", "a{1}"]), ("f:a{1\n".into(), String::new(), 0));
    assert_eq!(f.run(&["grep", "-e", "a\\{1\\}"]), ("f:a{1\n".into(), String::new(), 0));
    // Group 1 is closed by the time `\1` names it.
    assert_eq!(f.run(&["grep", "-e", "\\(o\\)\\1*k"]), ("f:ok\n".into(), String::new(), 0));
}
