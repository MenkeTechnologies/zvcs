//! `log`'s `unrecognized argument` comes after `setup_revisions()`.
//!
//! `cmd_log_init_finish()` runs `setup_revisions()` over the whole command line
//! and only then dies on the first argument nothing claimed:
//! `if (argc > 1) die(_("unrecognized argument: %s"), argv[1]);`
//! (builtin/log.c:316-320). Every revision has been resolved by then, so an
//! unresolvable one is reported instead, and the first commit parsed has read
//! `info/grafts` and printed its deprecation advice (commit.c:287-314). zvcs
//! died the moment it read the argument.
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
    fn empty(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-log-unrecognized-order-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        f
    }

    fn commit(&self, file: &str, body: &str, msg: &str) {
        std::fs::write(self.work.join(file), body).unwrap();
        self.run(&["add", file]);
        self.run(&["commit", "-q", "-m", msg]);
    }

    fn rev(&self, spec: &str) -> String {
        self.run(&["rev-parse", spec]).0.trim_end().to_string()
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
            .env("GIT_PAGER", "cat")
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

const HINT: &str = "hint: Support for <GIT_DIR>/info/grafts is deprecated
hint: and will be removed in a future Git version.
hint:
hint: Please use \"git replace --convert-graft-file\"
hint: to convert the grafts into replace refs.
hint:
hint: Turn this message off by running
hint: \"git config set advice.graftFileDeprecated false\"
";

/// Two commits; `info/grafts` makes the second a root.
fn grafted(tag: &str) -> Fixture {
    let f = Fixture::empty(tag);
    f.commit("a", "one\n", "one");
    f.commit("a", "two\n", "two");
    let two = f.rev("main");
    std::fs::write(f.work.join(".git/info/grafts"), format!("{two}\n")).unwrap();
    f
}

#[test]
fn the_graft_advice_precedes_the_refusal() {
    let f = grafted("hint");
    for (args, word) in [
        (&["log", "--bogus", "main"][..], "--bogus"),
        (&["log", "main", "--bogus", "--other"][..], "--bogus"),
        (&["log", "--timestamp", "main"][..], "--timestamp"),
        // The default `HEAD` is parsed too.
        (&["log", "--bogus"][..], "--bogus"),
    ] {
        assert_eq!(
            f.run(args),
            (String::new(), format!("{HINT}fatal: unrecognized argument: {word}\n"), 128),
            "{args:?}"
        );
    }
}

#[test]
fn a_bad_revision_after_it_is_reported_instead() {
    let f = grafted("badrev");
    let (out, err, code) = f.run(&["log", "--bogus", "nonexist"]);
    assert_eq!((out.as_str(), code), ("", 128));
    assert!(
        err.starts_with("fatal: ambiguous argument 'nonexist': unknown revision or path not in the working tree.\n"),
        "{err}"
    );
}
