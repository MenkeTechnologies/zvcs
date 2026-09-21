//! Four things `git tag` decides that this port had shaped differently:
//!
//!   * `check_tag_ref()` (refs.c:784-793) refuses a leading `-` and the exact
//!     name `HEAD` *before* the format check, neither of which
//!     `check_refname_format()` would reject on its own.
//!   * `--points-at` is `parse_opt_object_name()` (parse-options-cb.c:126-140),
//!     which *appends* to an `oid_array`; `match_points_at()`
//!     (ref-filter.c:2840-2862) then accepts a ref matching any entry, so
//!     repeated `--points-at` are OR-ed and `--no-points-at` clears them all.
//!   * `ref_sorting_set_sort_flags_all(sorting, REF_SORTING_ICASE, icase)`
//!     (builtin/tag.c:594) makes `-i` a sort flag, not only a match flag.
//!   * `versioncmp()`'s lazy config read (versioncmp.c:165-181) goes through
//!     `repo_config_get_string_multi()`, which *fails* on a valueless
//!     occurrence — printing `error: missing value for '<key>'` and returning
//!     -1 — so a bare `[versionsort] suffix` leaves the key unset and the
//!     deprecation warning unfired.
//!
//! Every expectation was measured from stock git 2.55.0 in an identical
//! throwaway repository under the same pinned environment.
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
    /// Two commits, so two tags can point at different objects.
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("zvcs-br-taglist-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "one\n").unwrap();
        f.git(&["add", "a"]);
        f.git(&["commit", "-q", "-m", "one"]);
        std::fs::write(f.work.join("a"), "two\n").unwrap();
        f.git(&["commit", "-q", "-am", "two"]);
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
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
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

    fn stdout(&self, args: &[&str]) -> String {
        let (out, err, code) = self.run(args);
        assert_eq!((err.as_str(), code), ("", 0), "`git {args:?}`");
        out
    }
}

/// `HEAD` and a leading `-` are refused as tag names, and no ref is written.
#[test]
fn head_and_a_leading_dash_are_not_valid_tag_names() {
    let f = Fixture::new("badname");

    for args in [
        vec!["tag", "HEAD"],
        vec!["tag", "-a", "-m", "useless", "HEAD"],
    ] {
        let (out, err, code) = f.run(&args);
        assert_eq!((out.as_str(), code), ("", 128), "{args:?}");
        assert_eq!(err, "fatal: 'HEAD' is not a valid tag name.\n", "{args:?}");
    }
    assert_eq!(f.run(&["rev-parse", "--verify", "--quiet", "refs/tags/HEAD"]).2, 1);

    let (out, err, code) = f.run(&["tag", "--", "-dash"]);
    assert_eq!((out.as_str(), code), ("", 128));
    assert_eq!(err, "fatal: '-dash' is not a valid tag name.\n");
}

/// Repeated `--points-at` are OR-ed, and `--no-points-at` drops the filter.
#[test]
fn repeated_points_at_are_or_ed_together() {
    let f = Fixture::new("pointsat");
    f.git(&["tag", "v1", "HEAD~"]);
    f.git(&["tag", "v2", "HEAD"]);
    f.git(&["tag", "v3", "HEAD"]);

    assert_eq!(f.stdout(&["tag", "--points-at=v1", "--points-at=v2"]), "v1\nv2\nv3\n");
    assert_eq!(f.stdout(&["tag", "--points-at", "v1"]), "v1\n");
    // `--no-points-at` clears the array, so every tag is listed again.
    assert_eq!(
        f.stdout(&["tag", "--points-at=v1", "--no-points-at", "--list"]),
        "v1\nv2\nv3\n"
    );
}

/// `-i` reorders as well as matching: with it, `TAG-two` sorts between
/// `initial` and `tag-one` rather than ahead of both.
#[test]
fn ignore_case_is_a_sort_flag_too() {
    let f = Fixture::new("icase");
    f.git(&["tag", "tag-one"]);
    f.git(&["tag", "TAG-two"]);
    f.git(&["tag", "initial"]);

    assert_eq!(f.stdout(&["tag", "-l"]), "TAG-two\ninitial\ntag-one\n");
    assert_eq!(f.stdout(&["tag", "-l", "-i"]), "initial\ntag-one\nTAG-two\n");
}

/// A valueless `versionsort` key reads as unset and reports itself, once per
/// key, instead of standing in for a configured suffix list.
#[test]
fn a_valueless_versionsort_key_is_reported_not_used() {
    let f = Fixture::new("versionsort");
    f.git(&["tag", "v1.0"]);
    f.git(&["tag", "v1.1"]);
    let cfg = f.work.join(".git/config");
    let mut text = std::fs::read_to_string(&cfg).unwrap();
    text.push_str("[versionsort]\n\tprereleaseSuffix\n\tsuffix\n");
    std::fs::write(&cfg, text).unwrap();

    let (out, err, code) = f.run(&["tag", "-l", "--sort=version:refname"]);
    assert_eq!((out.as_str(), code), ("v1.0\nv1.1\n", 0));
    assert_eq!(
        err,
        "error: missing value for 'versionsort.suffix'\n\
         error: missing value for 'versionsort.prereleasesuffix'\n"
    );
}
