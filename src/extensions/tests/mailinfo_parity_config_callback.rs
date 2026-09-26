//! `setup_mailinfo()` walks `git_mailinfo_config()` over the whole configuration.
//!
//! `cmd_mailinfo()` calls `setup_mailinfo()` before `parse_options()`
//! (builtin/mailinfo.c:86-88), and `setup_mailinfo()` ends with
//! `repo_config(r, git_mailinfo_config, mi)` (mailinfo.c:1286). The callback
//! (mailinfo.c:1252-1272) hands every key outside `mailinfo.` to
//! `git_default_config()`, reads `mailinfo.scissors` with `git_config_bool()`, and
//! refuses a valueless `mailinfo.quotedcr` with `config_error_nonbool()` and an
//! unknown action with `return error(...)` — both of which `configset_iter()`
//! turns into `git_die_config_linenr()`'s origin-naming fatal.
//!
//! zvcs read the two keys by last-value lookup: `mailinfo.scissors=bogus` died
//! with gitoxide's decoder message at exit 1, `1k` was not a boolean, the
//! `quotedcr` refusal was a bare `fatal:` with no `error:` line, a valueless
//! `quotedcr` was ignored, and `git_default_config()` never ran outside a
//! repository.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A message whose body carries a scissors line.
const MAIL: &str = "From: A <a@x>\nSubject: s\n\nbefore\n-- >8 --\nafter\n";

struct Fixture {
    root: PathBuf,
    work: PathBuf,
    outside: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-mailinfo-config-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        let outside = root.join("outside");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let f = Fixture { root, work, outside };
        f.run_in(&f.work, &["init", "-q", "-b", "main", "."]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        self.run_in(&self.work, args)
    }

    fn run_in(&self, dir: &PathBuf, args: &[&str]) -> (String, String, i32) {
        let mut child = Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // A refusal exits before reading stdin; a closed pipe is not a failure.
        let _ = child.stdin.take().unwrap().write_all(MAIL.as_bytes());
        let out = child.wait_with_output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn msg(&self, dir: &PathBuf) -> String {
        std::fs::read_to_string(dir.join("msg")).unwrap()
    }
}

fn died(stderr: &str) -> (String, String, i32) {
    (String::new(), stderr.to_string(), 128)
}

#[test]
fn a_bad_scissors_boolean_is_fatal_before_options_inside_and_outside_a_repository() {
    let f = Fixture::new("bool");
    let want = died("fatal: bad boolean config value 'bogus' for 'mailinfo.scissors'\n");
    for dir in [&f.work, &f.outside] {
        assert_eq!(f.run_in(dir, &["-c", "mailinfo.scissors=bogus", "mailinfo", "msg", "patch"]), want);
        assert_eq!(f.run_in(dir, &["-c", "mailinfo.scissors=bogus", "mailinfo", "-h"]), want);
        assert!(!dir.join("msg").exists());
    }
}

#[test]
fn scissors_takes_the_integer_grammar() {
    let f = Fixture::new("int");
    for dir in [&f.work, &f.outside] {
        let (_, err, code) = f.run_in(dir, &["-c", "mailinfo.scissors=1k", "mailinfo", "msg", "patch"]);
        assert_eq!((err.as_str(), code), ("", 0));
        assert_eq!(f.msg(dir), "after\n");
        let (_, _, code) = f.run_in(dir, &["-c", "mailinfo.scissors=0", "mailinfo", "msg", "patch"]);
        assert_eq!(code, 0);
        assert_eq!(f.msg(dir), "before\n-- >8 --\nafter\n");
    }
}

#[test]
fn quotedcr_refusals_are_errors_that_name_their_origin() {
    let f = Fixture::new("quotedcr");
    assert_eq!(
        f.run(&["-c", "mailinfo.quotedCr=bogus", "mailinfo", "msg", "patch"]),
        died(
            "error: bad action 'bogus' for 'mailinfo.quotedcr'\n\
             fatal: unable to parse 'mailinfo.quotedcr' from command-line config\n"
        )
    );
    assert_eq!(
        f.run(&["-c", "mailinfo.quotedCr", "mailinfo", "msg", "patch"]),
        died(
            "error: missing value for 'mailinfo.quotedcr'\n\
             fatal: unable to parse 'mailinfo.quotedcr' from command-line config\n"
        )
    );
    let config = f.work.join(".git/config");
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str("[mailinfo]\n\tquotedCr = nope\n");
    let line = text.lines().count();
    std::fs::write(&config, text).unwrap();
    assert_eq!(
        f.run(&["mailinfo", "msg", "patch"]),
        died(&format!(
            "error: bad action 'nope' for 'mailinfo.quotedcr'\n\
             fatal: bad config variable 'mailinfo.quotedcr' in file '.git/config' at line {line}\n"
        ))
    );
}

#[test]
fn the_default_callback_is_the_tail_of_one_walk_in_parse_order() {
    let f = Fixture::new("order");
    let scissors = died("fatal: bad boolean config value 'x' for 'mailinfo.scissors'\n");
    let abbrev =
        died("fatal: bad numeric config value 'bogus' for 'core.abbrev': invalid unit\n");
    let args = |first: &'static str, second: &'static str| {
        ["-c", first, "-c", second, "mailinfo", "msg", "patch"]
    };
    assert_eq!(f.run(&args("mailinfo.scissors=x", "core.abbrev=bogus")), scissors);
    assert_eq!(f.run(&args("core.abbrev=bogus", "mailinfo.scissors=x")), abbrev);
    // A later valid value does not rescue an earlier bad one.
    assert_eq!(f.run(&args("mailinfo.scissors=x", "mailinfo.scissors=true")), scissors);
    assert_eq!(
        f.run_in(&f.outside, &["-c", "core.abbrev=bogus", "mailinfo", "msg", "patch"]),
        abbrev
    );
}
