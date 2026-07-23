//! `git reflog show` honors `log.date` as the default *field* date format — the
//! one used for `%ad`/`%cd` and the `Date:`/`AuthorDate:`/`CommitDate:` header
//! lines — with `--date=` still overriding it. `log.date` never touches the reflog
//! selector column, which stays in count form. An invalid (or empty) `log.date` is
//! fatal at config read, ahead of any option or revision error, matching git.
//!
//! The `relative`/`human`/`format:...` modes need the current time or a strftime
//! user format that gix-date does not expose; they are deferred, so a command that
//! renders no field date still succeeds, and one that would render a field date
//! fails honestly rather than printing a wrong default-formatted date.
//!
//! `reflog expire`/`delete`/`drop` are not ported (gix-ref cannot rewrite a
//! reflog), so the `gc.reflogExpire*` keys have no code path to honor and are not
//! covered here — porting them would fabricate behavior the command does not have.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

/// A repo with two commits — so `HEAD` owns a reflog with two entries — at a fixed
/// author/commit date (1136214245 +0000 = 2006-01-02 15:04:05 UTC) so every field
/// date is deterministic across machines and git versions.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-reflogcfg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "alice@example.com"]);
    git(&repo, &["config", "user.name", "Alice"]);
    for (n, body) in [("c0", "a\n"), ("c1", "b\n")] {
        std::fs::write(repo.join("f"), body).unwrap();
        git(&repo, &["add", "f"]);
        // Pin the committer date too, so both `%cd` and the reflog entry timestamp
        // (which drives a `--date=` selector) are deterministic.
        let ok = Command::new("git")
            .args(["-c", "commit.gpgsign=false", "commit", "-q", "-m", n, "--date=1136214245 +0000"])
            .current_dir(&repo)
            .env("GIT_AUTHOR_DATE", "1136214245 +0000")
            .env("GIT_COMMITTER_DATE", "1136214245 +0000")
            .status()
            .unwrap()
            .success();
        assert!(ok, "commit {n} failed");
    }
    (repo, home)
}

fn reflog(repo: &Path, home: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["reflog"];
    args.extend_from_slice(extra);
    Command::new(BIN)
        .args(&args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .env("GIT_AUTHOR_DATE", "1136214245 +0000")
        .env("GIT_COMMITTER_DATE", "1136214245 +0000")
        .output()
        .unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn cfg(repo: &Path, home: &Path, value: &str) {
    Command::new(BIN)
        .args(["config", "log.date", value])
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .status()
        .unwrap();
}

#[test]
fn reflog_log_date_formats_field_dates() {
    let (repo, home) = fixture("field");

    // log.date=short → the `Date:` header line of `--pretty=medium` and the `%ad`
    // placeholder both render the fixed author date as an ISO short date.
    cfg(&repo, &home, "short");
    let d = stdout(&reflog(&repo, &home, &["--pretty=medium"]));
    assert!(d.contains("Date:   2006-01-02\n"), "log.date=short medium Date:\n{d}");
    let d = stdout(&reflog(&repo, &home, &["--format=%ad"]));
    assert_eq!(d, "2006-01-02\n2006-01-02\n", "log.date=short %ad:\n{d}");

    // log.date=unix reaches `%cd` too.
    cfg(&repo, &home, "unix");
    let d = stdout(&reflog(&repo, &home, &["--format=%cd"]));
    assert_eq!(d, "1136214245\n1136214245\n", "log.date=unix %cd:\n{d}");

    // log.date=iso-strict exercises the `Z` zero-offset fixup.
    cfg(&repo, &home, "iso-strict");
    let d = stdout(&reflog(&repo, &home, &["--format=%ad"]));
    assert_eq!(
        d, "2006-01-02T15:04:05Z\n2006-01-02T15:04:05Z\n",
        "log.date=iso-strict %ad:\n{d}"
    );

    // Fuller's AuthorDate/CommitDate lines take the same default.
    cfg(&repo, &home, "short");
    let d = stdout(&reflog(&repo, &home, &["--pretty=fuller"]));
    assert!(d.contains("AuthorDate: 2006-01-02\n"), "log.date=short fuller AuthorDate:\n{d}");
    assert!(d.contains("CommitDate: 2006-01-02\n"), "log.date=short fuller CommitDate:\n{d}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn reflog_date_flag_overrides_log_date() {
    let (repo, home) = fixture("override");
    cfg(&repo, &home, "short");

    // --date=iso on the command line overrides log.date=short for the field date.
    let d = stdout(&reflog(&repo, &home, &["--date=iso", "--pretty=medium"]));
    assert!(
        d.contains("Date:   2006-01-02 15:04:05 +0000\n"),
        "--date=iso must override log.date=short:\n{d}"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn reflog_log_date_leaves_selector_in_count_form() {
    let (repo, home) = fixture("selector");
    cfg(&repo, &home, "iso");

    // log.date only sets the field date format; the reflog selector column stays
    // in count form (`@{0}`), unlike an explicit `--date=` which switches it.
    let d = stdout(&reflog(&repo, &home, &[]));
    let first = d.lines().next().unwrap_or_default();
    assert!(first.contains("HEAD@{0}: commit: c1"), "selector stays count form:\n{d}");
    assert!(!first.contains("@{2006"), "log.date must not date the selector:\n{d}");

    // An explicit --date=iso, by contrast, does switch the selector to date form.
    let d = stdout(&reflog(&repo, &home, &["--date=iso"]));
    assert!(
        d.lines().next().unwrap_or_default().contains("HEAD@{2006-01-02 15:04:05 +0000}:"),
        "--date=iso must date the selector:\n{d}"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn reflog_log_date_invalid_is_fatal_before_argv() {
    let (repo, home) = fixture("bad");
    cfg(&repo, &home, "bogus");

    // git validates log.date in its log-config callback, before the argument scan,
    // so the config error is fatal (128) even with a valid --date present and even
    // ahead of an unrecognized option.
    for extra in [
        &["--date=iso", "--pretty=medium"][..],
        &["--totally-bogus-flag"][..],
        &[][..],
    ] {
        let out = reflog(&repo, &home, extra);
        assert_eq!(out.status.code(), Some(128), "invalid log.date must exit 128 for {extra:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("unknown date format bogus"),
            "expected git's error for {extra:?}:\n{err}"
        );
    }

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn reflog_log_date_empty_is_fatal() {
    let (repo, home) = fixture("empty");
    cfg(&repo, &home, "");

    // An empty value is unknown to git too (`fatal: unknown date format `), where a
    // naive parse would otherwise accept it as the default layout.
    let out = reflog(&repo, &home, &[]);
    assert_eq!(out.status.code(), Some(128), "empty log.date must exit 128");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("unknown date format "), "expected git's empty-value error:\n{err}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn reflog_log_date_relative_deferred_no_fabrication() {
    let (repo, home) = fixture("relative");
    cfg(&repo, &home, "relative");

    // `relative` needs the current time, which gix-date does not expose. A format
    // that renders no field date (the default oneline, and `--pretty=short`) is
    // unaffected, so the command still succeeds.
    let out = reflog(&repo, &home, &[]);
    assert_eq!(out.status.code(), Some(0), "relative log.date, no field date, must succeed");
    assert!(stdout(&out).contains("HEAD@{0}: commit: c1"), "oneline still rendered:\n{}", stdout(&out));

    let out = reflog(&repo, &home, &["--pretty=short"]);
    assert_eq!(out.status.code(), Some(0), "short has no field date, must succeed");

    // A format that *would* render a field date fails honestly rather than printing
    // a wrong default-formatted date — no fabricated output.
    let out = reflog(&repo, &home, &["--pretty=medium"]);
    assert_ne!(out.status.code(), Some(0), "relative log.date on medium must not succeed");
    assert!(
        !stdout(&out).contains("Date:"),
        "must not fabricate a Date: line for an unrenderable mode:\n{}",
        stdout(&out)
    );

    // An explicit --date=iso overrides the unrenderable log.date, so it succeeds.
    let out = reflog(&repo, &home, &["--date=iso", "--pretty=medium"]);
    assert_eq!(out.status.code(), Some(0), "--date=iso must override relative log.date");
    assert!(
        stdout(&out).contains("Date:   2006-01-02 15:04:05 +0000\n"),
        "--date=iso must render the field date:\n{}",
        stdout(&out)
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn reflog_decoration_and_relative_date_placeholders() {
    // Regression: git-fuzzy's log/reflog preview runs
    // `reflog --decorate=full --pretty=%D|%cr|%an`, which zvcs rejected on the
    // `%D` decoration and `%cr` relative-date atoms. Verify byte-for-byte against
    // real git with a pinned clock so relative output is reproducible.
    let (repo, home) = fixture("deco");
    git(&repo, &["tag", "v1"]);

    let now = "1136300000"; // fixed "now" so %cr / %ar are deterministic
    let run = |bin: &str, extra: &[&str]| -> String {
        let mut args = vec!["reflog"];
        args.extend_from_slice(extra);
        let out = Command::new(bin)
            .args(&args)
            .current_dir(&repo)
            .env("HOME", &home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("ZVCS_HOME", &home)
            .env("GIT_TEST_DATE_NOW", now)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    for extra in [
        &["show", "--pretty=%D|%cr|%an"][..],   // git-fuzzy's exact atoms
        &["--decorate=full", "--pretty=%D|%cr|%an"][..],
        &["show", "--pretty=%d|%an"][..],       // %d wraps in " (...)"
        &["show", "--pretty=%D"][..],           // blank line per undecorated entry
        &["show", "--pretty="][..],             // empty format prints nothing
        &["show", "--pretty=%cr"][..],
        &["show", "--pretty=%ci"][..],
        &["show", "--pretty=%ct"][..],
    ] {
        assert_eq!(
            run("git", extra),
            run(BIN, extra),
            "reflog {extra:?} must match real git byte-for-byte"
        );
    }

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
