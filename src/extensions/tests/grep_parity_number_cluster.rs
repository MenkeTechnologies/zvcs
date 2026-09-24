//! `-NUM` inside a short-option cluster. `cmd_grep()`'s table carries
//! `OPT_NUMBER_CALLBACK(…, context_callback)` (builtin/grep.c:1134) and no digit
//! short option, so `parse_short_opt()` hands any digit run at an option position
//! to it and keeps parsing behind the run (parse-options.c:444-458). The port only
//! recognised a word made entirely of digits, so `git grep -c1 foo` died with
//! ``unknown switch `1'`` (129) where stock counts matches.
//!
//! Measured against git 2.55.0.

use std::path::PathBuf;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Two tracked files: `a` with one match, `b` with two matches three lines apart,
/// so a one-line context separates them and a two-line one would not.
struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let dir = std::env::temp_dir().join(format!("zvcs-grepnum-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fx = Fixture { dir: dir.canonicalize().unwrap() };
        assert!(fx.run(&["init", "-q", "-b", "main"]).status.success());
        std::fs::write(fx.dir.join("a"), "foo\nbar\n").unwrap();
        std::fs::write(fx.dir.join("b"), "x\nfoo\ny\nz\nfoo\nw\n").unwrap();
        assert!(fx.run(&["add", "a", "b"]).status.success());
        fx
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .current_dir(&self.dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap()
    }

    fn grep(&self, args: &[&str]) -> String {
        let mut argv = vec!["grep", "--threads", "1"];
        argv.extend_from_slice(args);
        let out = self.run(&argv);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn a_digit_run_after_a_flag_is_the_context_shortcut() {
    let fx = Fixture::new("after");
    assert_eq!(fx.grep(&["-c1", "foo"]), "a:1\nb:2\n");
    assert_eq!(fx.grep(&["-n1c", "foo"]), "a:1\nb:2\n");
}

#[test]
fn parsing_resumes_behind_the_digit_run() {
    let fx = Fixture::new("before");
    assert_eq!(
        fx.grep(&["-1n", "foo"]),
        "a:1:foo\na-2-bar\n--\nb-1-x\nb:2:foo\nb-3-y\nb-4-z\nb:5:foo\nb-6-w\n"
    );
    // The run stops at the first non-digit, so `-1c2` is `-1`, `-c`, `-2`.
    assert_eq!(fx.grep(&["-1c2", "foo"]), "a:1\nb:2\n");
}
