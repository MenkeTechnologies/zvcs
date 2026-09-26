//! The notes config callbacks, which git runs once per configured value.
//!
//! `load_display_notes()` reads `notes.displayRef` through
//! `repo_config(the_repository, notes_display_config, …)` (notes.c:1116-1131).
//! The callback sees each configured value once, in configuration order, and
//! a valueless spelling is `config_error_nonbool()` (notes.c:987-999), which
//! `configset_iter()` turns into `git_die_config_linenr()` naming that
//! occurrence (config.c:1654-1673). zvcs read the key from the merged snapshot,
//! where a `-c notes.displayRef=<ref>` is delivered twice, so an unresolvable
//! ref was warned about twice; and the valueless form was taken as an empty
//! ref name and warned about instead of dying.
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
    /// One commit carrying a note in `refs/notes/commits`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-notes-config-callbacks-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "base\n").unwrap();
        f.run(&["add", "file"]);
        f.run(&["commit", "-q", "-m", "base"]);
        f.run(&["notes", "add", "-m", "yo"]);
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
            .env_remove("GIT_NOTES_DISPLAY_REF")
            .env_remove("GIT_NOTES_REWRITE_REF")
            .env_remove("GIT_NOTES_REWRITE_MODE")
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
fn one_command_line_display_ref_is_warned_about_once() {
    let f = Fixture::new("display-once");
    let (out, err, code) = f.run(&[
        "-c",
        "notes.displayRef=refs/notes/x",
        "log",
        "-1",
        "--format=%s%n%N",
    ]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("base\nyo\n\n", "warning: notes ref refs/notes/x is invalid\n", 0)
    );
    // Two values are two callback runs, and so two warnings.
    let (_, err, code) = f.run(&[
        "-c",
        "notes.displayRef=refs/notes/x",
        "-c",
        "notes.displayRef=refs/notes/y",
        "log",
        "-1",
    ]);
    assert_eq!(
        (err.as_str(), code),
        (
            "warning: notes ref refs/notes/x is invalid\n\
             warning: notes ref refs/notes/y is invalid\n",
            0
        )
    );
}

#[test]
fn a_valueless_display_ref_dies_naming_its_origin() {
    let f = Fixture::new("display-nonbool");
    let (out, err, code) = f.run(&["-c", "notes.displayRef", "show", "-s", "--format=%s%N"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (
            "",
            "error: missing value for 'notes.displayref'\n\
             fatal: unable to parse 'notes.displayref' from command-line config\n",
            128
        )
    );

    let config = f.work.join(".git/config");
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str("[notes]\n\tdisplayRef\n");
    let line = text.lines().count();
    std::fs::write(&config, text).unwrap();
    let (out, err, code) = f.run(&["log", "-1", "--format=%N"]);
    let want = format!(
        "error: missing value for 'notes.displayref'\n\
         fatal: bad config variable 'notes.displayref' in file '.git/config' at line {line}\n"
    );
    assert_eq!((out.as_str(), err.as_str(), code), ("", want.as_str(), 128));
    // Notes that are never loaded never run the callback.
    let (out, err, code) = f.run(&["log", "-1", "--format=%s"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("base\n", "", 0));
}
