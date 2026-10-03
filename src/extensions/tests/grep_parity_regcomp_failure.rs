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
//! The `regerror()` tail is the one stock git 2.56.0 prints on macOS, the same
//! text `--grep` and `-L` reproduce.
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

#[test]
fn regcomp_failures_name_origin_pattern_and_reason() {
    let f = Fixture::new("fail");
    let pats = f.root.join("pats");
    let pats = pats.to_str().unwrap();
    for (args, msg) in [
        (&["grep", "a\\{1"][..], "fatal: command line, 'a\\{1': braces not balanced\n".to_owned()),
        (&["grep", "-e", "a\\{1,"][..], "fatal: -e option, 'a\\{1,': braces not balanced\n".to_owned()),
        (&["grep", "-E", "-e", "a{1"][..], "fatal: -e option, 'a{1': braces not balanced\n".to_owned()),
        (&["grep", "-e", "ok", "-e", "["][..], "fatal: -e option, '[': brackets ([ ]) not balanced\n".to_owned()),
        (&["grep", "-e", "\\("][..], "fatal: -e option, '\\(': parentheses not balanced\n".to_owned()),
        (&["grep", "-e", "a\\{2,1\\}"][..], "fatal: -e option, 'a\\{2,1\\}': invalid repetition count(s)\n".to_owned()),
        // A back reference may only name a group that has closed.
        (&["grep", "-e", "\\(o\\)\\2"][..], "fatal: -e option, '\\(o\\)\\2': invalid backreference number\n".to_owned()),
        (&["grep", "-e", "\\(o\\1\\)"][..], "fatal: -e option, '\\(o\\1\\)': invalid backreference number\n".to_owned()),
        (&["grep", "-f", pats][..], format!("fatal: In '{pats}' at 2, 'x\\(': parentheses not balanced\n")),
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
