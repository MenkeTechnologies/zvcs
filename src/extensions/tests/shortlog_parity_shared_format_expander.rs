//! `shortlog --format` and `--group=format:` expand through `pretty.c`.
//!
//! `shortlog_add_commit()` builds a user-format `pretty_print_context` with
//! `ctx.abbrev`, `ctx.date_mode` and `ctx.output_encoding` set
//! (builtin/shortlog.c:243-251) and hands every format — the record's and each
//! `--group=format:` key's — to `repo_format_commit_message()` (:231, :257).
//! There is no shortlog-specific placeholder table: `%b`, `%(trailers)`, `%d`,
//! the `%+`/`%-`/`% ` magic and `--date=format:` all behave as under `git log`,
//! and an unknown `%Q` prints literally.
//!
//! zvcs carried a reduced expander of its own that refused each of those with
//! "`--format` placeholder … is not ported" and exited 1.
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
    /// `one` has a body and a `Reviewed-by` trailer; `two` is `main`, tagged `v2`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-shortlog-format-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.stdout(&["init", "-q", "-b", "main", "."]);
        f.stdout(&[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "one",
            "-m",
            "body line",
            "-m",
            "Reviewed-by: R Viewer <r@example.com>",
        ]);
        f.stdout(&["commit", "-q", "--allow-empty", "-m", "two"]);
        f.stdout(&["tag", "v2"]);
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
fn record_format_expands_body_magic_and_decorations() {
    let f = Fixture::new("record");
    // The record is one line: `insert_one_record()` folds the body onto it.
    assert_eq!(
        f.stdout(&["shortlog", "--format=%s%+b|", "main"]),
        "A U Thor (2):\n      one body line\n      two|\n\n"
    );
    // `%d` loads the ref decorations lazily, as `rev-list --format` does.
    assert_eq!(
        f.stdout(&["shortlog", "--format=%s%d", "main"]),
        "A U Thor (2):\n      one\n      two (HEAD -> main, tag: v2)\n\n"
    );
    assert_eq!(
        f.stdout(&["shortlog", "--format=[%(trailers:key=Reviewed-by,valueonly)]", "main"]),
        "A U Thor (2):\n      [R Viewer <r@example.com> ]\n      []\n\n"
    );
}

#[test]
fn record_format_reads_date_mode_and_abbrev() {
    let f = Fixture::new("knobs");
    assert_eq!(
        f.stdout(&["shortlog", "--date=format:%Y/%m", "--format=%s@%ad", "main"]),
        "A U Thor (2):\n      one@2023/11\n      two@2023/11\n\n"
    );
    let one = f.stdout(&["rev-parse", "main~1"]);
    let two = f.stdout(&["rev-parse", "main"]);
    assert_eq!(
        f.stdout(&["shortlog", "--abbrev=12", "--format=%h", "main"]),
        format!("A U Thor (2):\n      {}\n      {}\n\n", &one[..12], &two[..12])
    );
}

#[test]
fn group_format_keys_expand_the_same_way() {
    let f = Fixture::new("group");
    assert_eq!(
        f.stdout(&[
            "shortlog",
            "--group=format:%(trailers:key=Reviewed-by,valueonly,separator=)",
            "main",
        ]),
        " (1):\n      two\n\nR Viewer <r@example.com> (1):\n      one\n\n"
    );
    // An unknown placeholder is literal text, not an error.
    assert_eq!(f.stdout(&["shortlog", "-s", "--group=format:%Q", "main"]), "     2\t%Q\n");
}
