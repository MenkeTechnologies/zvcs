//! `format-patch`'s prefix, output and refusal paths.
//!
//! `--src-prefix`/`--dst-prefix` replace the `a/`+`b/` the `diff --git`, `---` and
//! `+++` lines carry; `--output=<file>` collects the whole series into one file, which
//! `OPT_FILENAME` creates while it parses — so it is left behind even when the
//! `--stdout` conflict kills the command. `--ignore-if-in-upstream` needs a two-endpoint
//! range and `--creation-factor` needs a `--range-diff`, both fatal before any output,
//! and anything `setup_revisions()` cannot place is `unrecognized argument`.
//!
//! Expectations measured against stock git 2.55.0.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output};

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
        let root = std::env::temp_dir().join(format!("zvcs-fpopts-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        std::fs::write(f.work.join("f.txt"), "a\n").unwrap();
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "seed"]);
        std::fs::write(f.work.join("f.txt"), "a\nb\n").unwrap();
        f.git(&["commit", "-q", "-am", "edit"]);
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
            .env_remove("GIT_AUTHOR_NAME")
            .env_remove("GIT_AUTHOR_EMAIL")
            .env_remove("GIT_COMMITTER_NAME")
            .env_remove("GIT_COMMITTER_EMAIL");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    fn text(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

/// The path prefixes are configurable, and `--default-prefix` puts `a/`+`b/` back.
#[test]
fn source_and_destination_prefixes_are_configurable() {
    let f = Fixture::new("prefix");
    let out = f.text(&[
        "format-patch",
        "--stdout",
        "-1",
        "--src-prefix=x/",
        "--dst-prefix=y/",
    ]);
    assert!(out.contains("diff --git x/f.txt y/f.txt"), "{out}");
    assert!(out.contains("--- x/f.txt\n+++ y/f.txt\n"), "{out}");

    // `--no-prefix` empties both; a later `--default-prefix` restores the defaults.
    let bare = f.text(&["format-patch", "--stdout", "-1", "--no-prefix"]);
    assert!(bare.contains("diff --git f.txt f.txt"), "{bare}");
    let restored = f.text(&[
        "format-patch",
        "--stdout",
        "-1",
        "--src-prefix=x/",
        "--default-prefix",
    ]);
    assert!(restored.contains("diff --git a/f.txt b/f.txt"), "{restored}");
}

/// `--output=<file>` writes the series to that file and announces nothing.
#[test]
fn output_collects_the_series_into_one_file() {
    let f = Fixture::new("output");
    let out = f.run(&["format-patch", "-2", "--output=series.patch"]);
    assert!(out.status.success(), "{out:?}");
    assert!(out.stdout.is_empty(), "nothing is announced: {out:?}");
    let body = std::fs::read_to_string(f.work.join("series.patch")).unwrap();
    assert_eq!(body.matches("\nSubject: [PATCH").count(), 2, "{body}");

    // `--stdout` and `--output` are mutually exclusive — but the file the option
    // opened is still created, because `OPT_FILENAME` opens it as it parses.
    let clash = f.run(&["format-patch", "--stdout", "-1", "--output=other.patch"]);
    assert_eq!(clash.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&clash.stderr),
        "fatal: options '--stdout' and '--output' cannot be used together\n"
    );
    assert_eq!(std::fs::read(f.work.join("other.patch")).unwrap().len(), 0);
}

/// The refusals git raises before writing anything, in git's own wording.
#[test]
fn option_refusals_match_git() {
    let f = Fixture::new("refuse");
    let cases: [(&[&str], &str); 4] = [
        (
            &["format-patch", "--stdout", "-1", "--ignore-if-in-upstream"],
            "fatal: need exactly one range\n",
        ),
        (
            &["format-patch", "--stdout", "-1", "--creation-factor=50"],
            "fatal: the option '--creation-factor' requires '--range-diff'\n",
        ),
        (
            &["format-patch", "--stdout", "-1", "--mbox"],
            "fatal: unrecognized argument: --mbox\n",
        ),
        (
            &["format-patch", "--stdout", "-1", "--no-such-option"],
            "fatal: unrecognized argument: --no-such-option\n",
        ),
    ];
    for (args, want) in cases {
        let out = f.run(args);
        assert_eq!(out.status.code(), Some(128), "{args:?}: {out:?}");
        assert_eq!(String::from_utf8_lossy(&out.stderr), want, "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}: {out:?}");
    }

    // A two-endpoint range is what `--ignore-if-in-upstream` wants; it gets past the
    // range check (the comparison itself is not ported, so it stops later).
    let ranged = f.run(&["format-patch", "--stdout", "--ignore-if-in-upstream", "HEAD~1..HEAD"]);
    assert!(
        !String::from_utf8_lossy(&ranged.stderr).contains("need exactly one range"),
        "{ranged:?}"
    );
}
