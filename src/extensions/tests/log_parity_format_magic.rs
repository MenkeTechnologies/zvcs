//! The `%-`, `%+` and `% ` magic of a user format.
//!
//! `format_commit_item()` (pretty.c:1906-1965) reads a `-`, `+` or ` ` between
//! the `%` and the placeholder: `%+x` inserts a line feed before a non-empty
//! expansion, `% x` a space, and `%-x` deletes the line feeds right before an
//! empty one. The item answers `consumed + 1` even when the placeholder itself is
//! unknown, so `%+Q` prints `Q` with no `%`, and `%+w(…)` is refused outright
//! (`return 0`) and prints literally. The magic is applied after
//! `format_and_pad_commit()` has laid out a pending `%<(N)` column, so `% s`
//! inside a padded field gains its space outside the padding.
//!
//! zvcs had no magic at all: `log`, `show` and `rev-list` printed every `%+b`,
//! `%-b` and `% an` literally.
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
    /// `one` carries a body, `two` (the tip) has none.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-log-format-magic-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        f.run(&["commit", "-q", "--allow-empty", "-m", "one", "-m", "body line"]);
        f.run(&["commit", "-q", "--allow-empty", "-m", "two"]);
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
            .env("GIT_PAGER", "cat")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn stdout(&self, args: &[&str]) -> String {
        let (out, err, code) = self.run(args);
        assert_eq!((err.as_str(), code), ("", 0), "{args:?}");
        out
    }
}

#[test]
fn plus_adds_a_line_feed_only_before_a_non_empty_expansion() {
    let f = Fixture::new("plus");
    assert_eq!(f.stdout(&["log", "--format=%s%+b|"]), "two|\none\nbody line\n|\n");
    // Under `--graph` the inserted line feed starts a new, indented graph line.
    assert_eq!(f.stdout(&["log", "--graph", "--format=%s%+b"]), "* two\n* one\n  body line\n  \n");
    // `rev-list` renders through the same user-format expander.
    let tip = f.stdout(&["rev-parse", "main"]);
    let base = f.stdout(&["rev-parse", "main~1"]);
    assert_eq!(
        f.stdout(&["rev-list", "--format=%s%+b", "main"]),
        format!("commit {}two\ncommit {}one\nbody line\n\n", tip, base)
    );
}

#[test]
fn minus_deletes_the_line_feeds_before_an_empty_expansion() {
    let f = Fixture::new("minus");
    assert_eq!(f.stdout(&["log", "--format=%s%n%-b|"]), "two|\none\nbody line\n|\n");
}

#[test]
fn space_adds_a_space_outside_a_padded_field() {
    let f = Fixture::new("space");
    assert_eq!(f.stdout(&["log", "-1", "--format=[% an]%+%"]), "[ A U Thor]%\n");
    assert_eq!(f.stdout(&["show", "-s", "--format=%<(6)% s|"]), " two   |\n");
}

#[test]
fn magic_before_an_unknown_placeholder_swallows_the_percent() {
    let f = Fixture::new("unknown");
    assert_eq!(f.stdout(&["log", "-1", "--format=[%+Q][%-Q][% Q]"]), "[Q][Q][Q]\n");
    // A magic character with nothing after it is consumed alone.
    assert_eq!(f.stdout(&["log", "-1", "--format=[%+"]), "[\n");
}

#[test]
fn magic_before_a_wrap_atom_is_refused_and_printed_literally() {
    let f = Fixture::new("wrap");
    assert_eq!(f.stdout(&["log", "-1", "--format=[%+w(4)x]"]), "[%+w(4)x]\n");
}
