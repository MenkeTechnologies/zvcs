//! `--show-notes-by-default` on `log` and `show`.
//!
//! `revs->show_notes_by_default` (revision.c:2599-2600) is read at the end of
//! `setup_revisions()`: with no `--notes`/`--no-notes` of its own the run
//! enables the default notes display and counts it as given
//! (revision.c:3217-3220), so a format like `--oneline`, which alone shows no
//! notes (builtin/log.c:328-329), shows them. A user format still has to ask
//! with `%N`. zvcs refused the option as unsupported.
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
        let root = std::env::temp_dir().join(format!("zvcs-show-notes-by-default-{tag}-{}", std::process::id()));
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

fn noted(tag: &str) -> Fixture {
    let f = Fixture::empty(tag);
    f.commit("a", "one\n", "one");
    f.commit("a", "two\n", "two");
    f.run(&["notes", "add", "-m", "a note", "main"]);
    f
}

#[test]
fn a_format_without_notes_gets_them() {
    let f = noted("oneline");
    let short = f.run(&["rev-parse", "--short", "main"]).0.trim_end().to_string();
    let with_notes = format!("{short} two\nNotes:\n    a note\n\n");
    for verb in [&["log", "-1"][..], &["show", "-s"][..]] {
        let mut args = verb.to_vec();
        args.extend_from_slice(&["--show-notes-by-default", "--oneline", "main"]);
        assert_eq!(f.run(&args), (with_notes.clone(), String::new(), 0), "{args:?}");
    }
    assert_eq!(f.run(&["log", "-1", "--oneline", "main"]).0, format!("{short} two\n"));
}

#[test]
fn an_explicit_choice_or_a_user_format_wins() {
    let f = noted("explicit");
    let short = f.run(&["rev-parse", "--short", "main"]).0.trim_end().to_string();
    assert_eq!(
        f.run(&["log", "-1", "--no-notes", "--show-notes-by-default", "--oneline", "main"]).0,
        format!("{short} two\n")
    );
    assert_eq!(f.run(&["log", "-1", "--show-notes-by-default", "--format=%s", "main"]).0, "two\n");
}
