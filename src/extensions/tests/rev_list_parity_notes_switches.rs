//! `rev-list` refused notes at the first switch rather than by the final state.
//!
//! `handle_revision_opt()` only records the notes switches — `--notes` and
//! `--notes=<ref>` turn `revs->show_notes` on, `--no-notes` turns it off,
//! `--standard-notes` picks refs without turning anything on, and
//! `--show-notes-by-default` turns it on at the end of `setup_revisions()`
//! unless a switch was given (revision.c:2584-2616, 3217-3220).
//! `cmd_rev_list()` dies on the final `revs.show_notes`, after the argument
//! and usage checks (builtin/rev-list.c:898-906). zvcs died at the first
//! turning-on spelling, so `--notes --no-notes` failed, `--standard-notes`
//! failed, and `--show-notes-by-default` was a usage error.
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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-rev-list-notes-switches-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "A"]);
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
fn the_final_state_decides() {
    let f = Fixture::new("state");
    let head = f.run(&["rev-parse", "main"]).0;
    for args in [
        &["--notes", "--no-notes"][..],
        &["--notes=x", "--no-notes"],
        &["--standard-notes"],
        &["--show-notes-by-default", "--no-notes"],
        &["--no-notes", "--show-notes-by-default"],
    ] {
        let mut argv = vec!["rev-list"];
        argv.extend_from_slice(args);
        argv.push("main");
        assert_eq!(f.run(&argv), (head.clone(), String::new(), 0), "{args:?}");
    }
    let refused = (String::new(), "fatal: rev-list does not support display of notes\n".to_string(), 128);
    for args in [&["--no-notes", "--notes"][..], &["--show-notes-by-default"], &["--show-notes=x"]] {
        let mut argv = vec!["rev-list"];
        argv.extend_from_slice(args);
        argv.push("main");
        assert_eq!(f.run(&argv), refused, "{args:?}");
    }
    // The refusal comes after the usage check.
    assert_eq!(f.run(&["rev-list", "--notes"]).2, 129);
}
