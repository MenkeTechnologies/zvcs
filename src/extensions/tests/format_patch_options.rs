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

/// Every name `parse_algorithm_value()` accepts (`diff.c`), on content where
/// patience genuinely disagrees with the other three.
///
/// The fixture is the point. `patience`, `myers`, `minimal` and `histogram` agree
/// on almost any small edit, so a test built on a one-line change proves nothing
/// about which algorithm ran — it passes just as happily when the flag is
/// silently ignored. These two revisions are chosen so stock 2.55.0 emits a
/// *different set of hunks* for patience than for the other three:
///
/// ```text
/// before: c b c d c a c        after: b c a c a b b d
/// ```
///
/// which is what makes this a regression test for the port having pushed
/// `--diff-algorithm=patience` onto its deferred list — over a comment claiming
/// the vendored imara-diff had no patience implementation, while
/// `gix-imara-diff`'s `patience.rs` is a port of `xdiff/xpatience.c` and
/// `Algorithm::Patience` is wired straight through `diff()`.
#[test]
fn every_diff_algorithm_name_selects_the_algorithm_it_names() {
    let f = Fixture::new("algo");
    std::fs::write(f.work.join("f.txt"), "c\nb\nc\nd\nc\na\nc\n").unwrap();
    f.git(&["commit", "-q", "-am", "base"]);
    std::fs::write(f.work.join("f.txt"), "b\nc\na\nc\na\nb\nb\nd\n").unwrap();
    f.git(&["commit", "-q", "-am", "ours"]);

    // `-U0` so the hunk headers are the algorithm's own choice of anchors and not
    // three lines of shared context around them.
    let hunks = |extra: &[&str]| -> Vec<String> {
        let mut args = vec!["format-patch", "-1", "--stdout", "-U0"];
        args.extend_from_slice(extra);
        args.push("HEAD");
        f.text(&args)
            .lines()
            .filter(|l| l.starts_with("@@"))
            .map(str::to_owned)
            .collect()
    };

    let myers = ["@@ -1 +0,0 @@", "@@ -4,2 +2,0 @@ c", "@@ -7,0 +5,4 @@ c"];
    let patience = ["@@ -1 +0,0 @@", "@@ -3,0 +3,5 @@ c", "@@ -5,3 +8,0 @@ d"];
    assert_ne!(myers, patience, "the fixture has to discriminate");

    // The default, and the four names that reproduce it. `default` is
    // `parse_algorithm_value()`'s own spelling of "clear every algorithm bit".
    for extra in [
        &[][..],
        &["--diff-algorithm=myers"][..],
        &["--diff-algorithm=minimal"][..],
        &["--diff-algorithm=histogram"][..],
        &["--diff-algorithm=default"][..],
        &["--minimal"][..],
        &["--histogram"][..],
    ] {
        assert_eq!(hunks(extra), myers, "{extra:?}");
    }

    // Patience, by both spellings — and `strcasecmp`, so the case of the value is
    // not part of the name.
    for extra in [
        &["--diff-algorithm=patience"][..],
        &["--patience"][..],
        &["--diff-algorithm=PATIENCE"][..],
    ] {
        assert_eq!(hunks(extra), patience, "{extra:?}");
    }

    // One knob, so the last spelling on the line wins whichever form it took.
    assert_eq!(hunks(&["--patience", "--histogram"]), myers);
    assert_eq!(hunks(&["--histogram", "--patience"]), patience);
    assert_eq!(
        hunks(&["--diff-algorithm=patience", "--diff-algorithm=myers"]),
        myers
    );

    // A name it does not take is parse-options' bare `exit(129)`: an `error:`
    // line and no usage block. An empty value is not a name either.
    for value in ["bogus", "", "Patiencex"] {
        let out = f.run(&[
            "format-patch",
            "-1",
            "--stdout",
            &format!("--diff-algorithm={value}"),
            "HEAD",
        ]);
        assert_eq!(out.status.code(), Some(129), "{value:?}: {out:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            "error: option diff-algorithm accepts \"myers\", \"minimal\", \
             \"patience\" and \"histogram\"\n",
            "{value:?}"
        );
        assert!(out.stdout.is_empty(), "{value:?}: {out:?}");
    }
}
