//! An option written after the first path operand of `log`/`show`.
//!
//! `setup_revisions()` leaves its option loop at the first operand that is a
//! path rather than a revision, and checks the rest of argv as paths:
//!
//! ```c
//! for (j = i; j < argc; j++)
//!         verify_filename(the_repository, revs->prefix, argv[j], j == i);
//! ```
//! (`revision.c:3127-3129`, v2.56.0)
//!
//! and `verify_filename()` refuses anything that starts with `-`:
//!
//! ```c
//! if (*arg == '-')
//!         die(_("option '%s' must come before non-option arguments"), arg);
//! ```
//! (`setup.c:285-286`)
//!
//! So `git log <path> --oneline` dies, where the port parsed every option
//! wherever it stood. The options of `cmd_log_init_finish()`'s own
//! `parse_options()` pass (`-q`, `--source`, `--decorate*`, `--use-mailmap`,
//! `-L`, ..., builtin/log.c:280-312) are taken out of argv before
//! `setup_revisions()` runs, so those still work after a path; so does anything
//! after `--`, which is a path.
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
    /// Two commits, both touching `f`; `g` is added by the second.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-log-optpath-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("f"), "a\n").unwrap();
        f.git(&["add", "f"]);
        f.git(&["commit", "-q", "-m", "one"]);
        std::fs::write(f.work.join("f"), "b\n").unwrap();
        std::fs::write(f.work.join("g"), "c\n").unwrap();
        f.git(&["add", "f", "g"]);
        f.git(&["commit", "-q", "-m", "two"]);
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
            .env("TZ", "UTC")
            .env("GIT_PAGER", "cat")
            .stdin(std::process::Stdio::null());
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = self.cmd(args).output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

fn must_come_before(opt: &str) -> String {
    format!("fatal: option '{opt}' must come before non-option arguments\n")
}

#[test]
fn a_revision_option_after_a_path_dies() {
    let f = Fixture::new("dies");
    for (args, opt) in [
        (&["log", "f", "--oneline"][..], "--oneline"),
        (&["log", "f", "-p"][..], "-p"),
        (&["log", "f", "--since=1"][..], "--since=1"),
        // A separate-valued option is named, not its value.
        (&["log", "f", "--grep", "two"][..], "--grep"),
        // The first offender in argv order wins, path or option.
        (&["log", "f", "--all", "nosuch"][..], "--all"),
        (&["log", "f", "g", "--stdin"][..], "--stdin"),
        (&["log", "f", "--end-of-options"][..], "--end-of-options"),
        (&["whatchanged", "--i-still-use-this", "f", "--oneline"][..], "--oneline"),
        (&["show", "f", "--oneline"][..], "--oneline"),
        (&["show", "f", "g", "--stat"][..], "--stat"),
    ] {
        let (out, err, code) = f.run(args);
        assert_eq!((out.as_str(), err, code), ("", must_come_before(opt), 128), "{args:?}");
    }
    // A missing path standing before the option is reported first.
    let (_, err, code) = f.run(&["log", "f", "nosuch", "--oneline"]);
    assert_eq!(code, 128);
    assert!(err.starts_with("fatal: nosuch: no such path in the working tree."), "{err}");
}

#[test]
fn log_parse_options_and_paths_after_dashdash_still_work() {
    let f = Fixture::new("works");
    let (base, err, code) = f.run(&["log", "--format=%s", "f"]);
    assert_eq!((base.as_str(), err.as_str(), code), ("two\none\n", "", 0));
    for extra in [
        &["-q"][..],
        &["--source"][..],
        &["--no-source"][..],
        &["--decorate"][..],
        &["--decorate=full"][..],
        &["--no-decorate"][..],
        &["--decorate-refs", "main"][..],
        &["--decorate-refs-exclude=main"][..],
        &["--clear-decorations"][..],
        &["--use-mailmap"][..],
        &["--no-mailmap"][..],
    ] {
        let args = [&["log", "--format=%s", "f"][..], extra].concat();
        let (out, err, code) = f.run(&args);
        assert_eq!((out.as_str(), err.as_str(), code), (base.as_str(), "", 0), "{args:?}");
    }
    // Options ahead of the path, and tokens after `--`, are not the tail.
    let (out, err, code) = f.run(&["log", "--oneline", "--format=%s", "HEAD", "--", "g", "--oneline"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("two\n", "", 0));
    let (out, err, code) = f.run(&["show", "-s", "--format=%s", "f", "--source"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("two\n", "", 0));
}
