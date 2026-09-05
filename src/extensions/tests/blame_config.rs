//! `git blame` honors `blame.showEmail` as the default for `-e`/`--show-email`,
//! with the command line still overriding (`--no-show-email`). Regression guard
//! for the config being ignored (author name always shown).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Run stock git in `dir` with the ambient identity stripped.
///
/// The fixture below establishes `Alice` through the repository's own config,
/// and the blame output asserted on is that author name. git resolves the
/// identity from `GIT_AUTHOR_*`/`GIT_COMMITTER_*` ahead of any config, so an
/// environment that exports them — CI does, to give the runner an identity at
/// all — would author the fixture commit as someone else and every assertion
/// here would read that name instead. Removing the four variables hands the
/// decision back to the config this test is about.
fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env_remove("GIT_AUTHOR_NAME")
            .env_remove("GIT_AUTHOR_EMAIL")
            .env_remove("GIT_COMMITTER_NAME")
            .env_remove("GIT_COMMITTER_EMAIL")
            .status()
            .unwrap()
            .success(),
        "git {args:?} failed"
    );
}

fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-blamecfg-{tag}-{}", std::process::id()));
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
    std::fs::write(repo.join("f"), "hello\n").unwrap();
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-q", "-m", "c0"]);
    (repo, home)
}

fn blame(repo: &Path, home: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["blame"];
    args.extend_from_slice(extra);
    args.push("f");
    Command::new(BIN)
        .args(&args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .output()
        .unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn blame_show_email_config_and_override() {
    let (repo, home) = fixture("showemail");

    // Default: author name.
    let d = stdout(&blame(&repo, &home, &[]));
    assert!(d.contains("Alice"), "default shows the name:\n{d}");
    assert!(!d.contains("<alice@example.com>"), "default hides the email:\n{d}");

    // blame.showEmail=true → email column.
    git(&repo, &["config", "blame.showEmail", "true"]);
    let d = stdout(&blame(&repo, &home, &[]));
    assert!(d.contains("<alice@example.com>"), "config should show the email:\n{d}");

    // --no-show-email overrides the config back to the name.
    let d = stdout(&blame(&repo, &home, &["--no-show-email"]));
    assert!(d.contains("Alice"), "--no-show-email must override config:\n{d}");
    assert!(!d.contains("<alice@example.com>"), "email suppressed by override:\n{d}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

// ---------------------------------------------------------------------------
// blame.date / `--date=<mode>`: the default date format for the human-format
// timestamp column, overridable on the command line. git validates the mode at
// config-read time (fatal, exit 128) exactly like the CLI flag.
// ---------------------------------------------------------------------------

/// Single-commit fixture with a fixed author/committer date so the blamed
/// timestamp is deterministic across machines and runs.
fn dated_fixture(tag: &str, date: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-blamedate-{tag}-{}", std::process::id()));
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
    std::fs::write(repo.join("f"), "hello\n").unwrap();
    git(&repo, &["add", "f"]);
    assert!(
        Command::new(BIN)
            .args(["commit", "-q", "-m", "c0"])
            .current_dir(&repo)
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date)
            .status()
            .unwrap()
            .success(),
        "dated commit failed"
    );
    (repo, home)
}

/// Run `git blame [extra] f` in `repo` under an isolated, deterministic
/// environment. `bin` is either the zvcs binary or the system `git`, run with
/// byte-identical env so their outputs are directly comparable.
fn run_blame(bin: &str, repo: &Path, home: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["blame"];
    args.extend_from_slice(extra);
    args.push("f");
    Command::new(bin)
        .args(&args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("ZVCS_HOME", home)
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .unwrap()
}

fn zvcs_blame(repo: &Path, home: &Path, extra: &[&str]) -> Output {
    run_blame(BIN, repo, home, extra)
}

fn real_blame(repo: &Path, home: &Path, extra: &[&str]) -> Output {
    run_blame("git", repo, home, extra)
}

#[test]
fn blame_date_modes_match_git() {
    // UTC commit: `iso-strict` renders the `Z` zone and is shorter than its
    // fixed column width, exercising both the Z-form and left-justified padding.
    let (repo, home) = dated_fixture("modes", "1700000000 +0000");

    for m in [
        "iso",
        "iso8601",
        "iso-strict",
        "iso8601-strict",
        "short",
        "raw",
        "unix",
        "rfc",
        "rfc2822",
        "default",
    ] {
        let flag = format!("--date={m}");
        let z = zvcs_blame(&repo, &home, &[&flag]);
        let g = real_blame(&repo, &home, &[&flag]);
        assert!(
            z.status.success(),
            "zvcs --date={m} failed: {}",
            String::from_utf8_lossy(&z.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&g.stdout),
            String::from_utf8_lossy(&z.stdout),
            "--date={m} must match git byte-for-byte"
        );
    }

    // Separate-argument form (`--date short`) is accepted like git's.
    let z = zvcs_blame(&repo, &home, &["--date", "short"]);
    let g = real_blame(&repo, &home, &["--date", "short"]);
    assert_eq!(
        String::from_utf8_lossy(&g.stdout),
        String::from_utf8_lossy(&z.stdout),
        "`--date short` must match git"
    );

    // No flag and no config defaults to iso8601, matching git's blame default.
    let z = zvcs_blame(&repo, &home, &[]);
    let g = real_blame(&repo, &home, &[]);
    assert_eq!(
        String::from_utf8_lossy(&g.stdout),
        String::from_utf8_lossy(&z.stdout),
        "default date column must match git"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn blame_date_config_default_and_override() {
    let (repo, home) = dated_fixture("config", "1700000000 +0000");

    // blame.date supplies the default mode.
    git(&repo, &["config", "blame.date", "short"]);
    let z = zvcs_blame(&repo, &home, &[]);
    let g = real_blame(&repo, &home, &[]);
    assert_eq!(
        String::from_utf8_lossy(&g.stdout),
        String::from_utf8_lossy(&z.stdout),
        "blame.date=short must apply and match git"
    );
    assert!(
        stdout(&z).contains("2023-11-14 1)"),
        "short is YYYY-MM-DD only:\n{}",
        stdout(&z)
    );

    // `--date` overrides blame.date.
    let z = zvcs_blame(&repo, &home, &["--date=raw"]);
    let g = real_blame(&repo, &home, &["--date=raw"]);
    assert_eq!(
        String::from_utf8_lossy(&g.stdout),
        String::from_utf8_lossy(&z.stdout),
        "--date must override blame.date and match git"
    );
    assert!(
        stdout(&z).contains("1700000000 +0000"),
        "override to raw:\n{}",
        stdout(&z)
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn blame_date_invalid_is_fatal() {
    let (repo, home) = dated_fixture("invalid", "1700000000 +0000");

    // Unknown `--date` mode: git's exact fatal and exit code.
    let z = zvcs_blame(&repo, &home, &["--date=bogus"]);
    assert_eq!(z.status.code(), Some(128), "invalid --date exits 128");
    assert_eq!(
        String::from_utf8_lossy(&z.stderr),
        "fatal: unknown date format bogus\n"
    );

    // Empty value is also unknown (matches git's empty-format message).
    let z = zvcs_blame(&repo, &home, &["--date="]);
    assert_eq!(z.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&z.stderr),
        "fatal: unknown date format \n"
    );

    // git validates blame.date at read time, so an invalid config value is
    // fatal even when a valid `--date` override is also present.
    git(&repo, &["config", "blame.date", "nope"]);
    let z = zvcs_blame(&repo, &home, &["--date=raw"]);
    assert_eq!(
        z.status.code(),
        Some(128),
        "invalid blame.date is fatal regardless of --date"
    );
    assert_eq!(
        String::from_utf8_lossy(&z.stderr),
        "fatal: unknown date format nope\n"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// The `-local` spellings and `format:<strftime>` — the modes that need
/// `localtime_r()` and the platform `strftime(3)` rather than this file's own
/// calendar arithmetic.
///
/// blame renders them through [`crate::showdate`], the shared port of `date.c`'s
/// `show_date()`, which is the same renderer every other verb answers `--date=`
/// with; `blame_date_width` measures `format:` by rendering the epoch through it
/// (`builtin/blame.c:1023`). They used to be refused outright, and this test
/// asserted the refusal — so it is the guard against that regressing back.
#[test]
fn blame_date_local_and_strftime_modes_match_git() {
    let (repo, home) = dated_fixture("unsup", "1700000000 +0000");

    // `run_blame` pins `TZ=UTC`, so `<mode>-local` renders in the same zone the
    // object header carries and each pair below has to agree byte for byte. The
    // oracle is the port's own non-local rendering rather than the `git` on PATH:
    // on a machine where that `git` is an older zvcs release these modes are the
    // very ones it refuses, so it answers with nothing and proves nothing.
    // `iso-local` keeps the zone field, so under `TZ=UTC` it is `iso` exactly.
    let l = zvcs_blame(&repo, &home, &["--date=iso-local"]);
    let p = zvcs_blame(&repo, &home, &["--date=iso"]);
    assert!(
        l.status.success(),
        "--date=iso-local failed: {}",
        String::from_utf8_lossy(&l.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&p.stdout),
        String::from_utf8_lossy(&l.stdout),
        "under TZ=UTC, --date=iso-local renders what --date=iso does"
    );

    // `default-local` is `DATE_NORMAL` with `local` set, and `show_date()`
    // (date.c) prints no zone at all for that pair — the column keeps its width,
    // so the calendar text is `default`'s and the `+0000` is gone. Verified
    // against git 2.55.0: `Wed Jan 1 00:00:00 2020 +0000 ` vs
    // `Wed Jan 1 00:00:00 2020       `.
    let dl = zvcs_blame(&repo, &home, &["--date=default-local"]);
    let d = zvcs_blame(&repo, &home, &["--date=default"]);
    assert!(
        dl.status.success(),
        "--date=default-local failed: {}",
        String::from_utf8_lossy(&dl.stderr)
    );
    let dl_out = String::from_utf8_lossy(&dl.stdout).into_owned();
    let d_out = String::from_utf8_lossy(&d.stdout).into_owned();
    assert!(!dl_out.contains("+0000"), "--date=default-local prints no zone:\n{dl_out}");
    assert!(d_out.contains("+0000"), "--date=default does print one:\n{d_out}");
    assert_eq!(
        d_out.replace("+0000", "     "),
        dl_out,
        "--date=default-local is --date=default with the zone field blanked"
    );

    // `format:<strftime>` reaches the platform `strftime(3)`, and its column is
    // `strlen(show_date(0, 0, &blame_date_mode))` wide (`builtin/blame.c:1023`) —
    // so the day it prints is the day `--date=short` prints, re-punctuated.
    let f = zvcs_blame(&repo, &home, &["--date=format:%Y/%m/%d"]);
    assert!(
        f.status.success(),
        "--date=format:%Y/%m/%d failed: {}",
        String::from_utf8_lossy(&f.stderr)
    );
    let short = zvcs_blame(&repo, &home, &["--date=short"]);
    let day = String::from_utf8_lossy(&short.stdout)
        .split_whitespace()
        .find(|w| w.len() == 10 && w.as_bytes()[4] == b'-')
        .expect("--date=short prints a YYYY-MM-DD column")
        .replace('-', "/");
    assert!(
        String::from_utf8_lossy(&f.stdout).contains(&day),
        "--date=format:%Y/%m/%d must render {day}:\n{}",
        String::from_utf8_lossy(&f.stdout)
    );

    // `format` without a colon is git's missing-separator fatal (exit 128).
    let z = zvcs_blame(&repo, &home, &["--date=format"]);
    assert_eq!(z.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&z.stderr),
        "fatal: date format missing colon separator: format\n"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

// ---------------------------------------------------------------------------
// Output-shaping flags that must match git byte-for-byte. The single-commit
// fixture's only commit is a root, so it is a boundary by default — which
// exercises `-b` (blank the name), `--root` (drop the boundary), `-c`
// (annotate-compat, no caret) and `-t` (raw timestamp) directly.
// ---------------------------------------------------------------------------

#[test]
fn blame_boundary_and_output_flags_match_git() {
    let (repo, home) = dated_fixture("outflags", "1700000000 +0000");

    for extra in [
        &["-b"][..],
        &["--root"][..],
        &["--no-root"][..],
        &["-t"][..],
        &["-c"][..],
        &["-c", "-e"][..],
        &["-c", "-t"][..],
        &["-l"][..],
        &["-b", "-l"][..],
    ] {
        let z = zvcs_blame(&repo, &home, extra);
        let g = real_blame(&repo, &home, extra);
        assert!(
            z.status.success(),
            "zvcs blame {extra:?} failed: {}",
            String::from_utf8_lossy(&z.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&g.stdout),
            String::from_utf8_lossy(&z.stdout),
            "blame {extra:?} must match git byte-for-byte"
        );
    }

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn blame_diff_algorithm_matches_git_and_rejects_unknown() {
    let (repo, home) = dated_fixture("diffalgo", "1700000000 +0000");

    for algo in ["myers", "default", "minimal", "histogram"] {
        let z = zvcs_blame(&repo, &home, &["--diff-algorithm", algo]);
        let g = real_blame(&repo, &home, &["--diff-algorithm", algo]);
        assert!(
            z.status.success(),
            "zvcs --diff-algorithm {algo} failed: {}",
            String::from_utf8_lossy(&z.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&g.stdout),
            String::from_utf8_lossy(&z.stdout),
            "--diff-algorithm {algo} must match git"
        );
    }

    // `--diff-algorithm=histogram` (glued form) is accepted too.
    let z = zvcs_blame(&repo, &home, &["--diff-algorithm=histogram"]);
    assert!(z.status.success(), "glued --diff-algorithm= form must parse");

    // An unknown algorithm is rejected (git dies too).
    let z = zvcs_blame(&repo, &home, &["--diff-algorithm", "bogus"]);
    assert!(!z.status.success(), "unknown --diff-algorithm must be rejected");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn blame_contents_matches_git() {
    let (repo, home) = dated_fixture("contents", "1700000000 +0000");

    // Identical content: every line still resolves to the committed blob, so the
    // output is fully deterministic and must match git byte-for-byte.
    std::fs::write(repo.join("f.same"), "hello\n").unwrap();
    let z = zvcs_blame(&repo, &home, &["--contents", "f.same"]);
    let g = real_blame(&repo, &home, &["--contents", "f.same"]);
    assert!(
        z.status.success(),
        "zvcs --contents failed: {}",
        String::from_utf8_lossy(&z.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&g.stdout),
        String::from_utf8_lossy(&z.stdout),
        "--contents (identical image) must match git byte-for-byte"
    );

    // Divergent content: the added line is attributed to git's synthetic
    // `External file (--contents)` author (the timestamp is "now", hence not
    // compared for exact equality).
    std::fs::write(repo.join("f.diff"), "hello\nextra\n").unwrap();
    let z = zvcs_blame(&repo, &home, &["--contents", "f.diff"]);
    let out = stdout(&z);
    assert!(z.status.success(), "divergent --contents must succeed");
    assert!(
        out.contains("External file (--contents)"),
        "added line uses git's --contents author identity:\n{out}"
    );
    assert!(out.contains("extra"), "added line content present:\n{out}");
    assert!(out.contains("hello"), "committed line still shown:\n{out}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn blame_date_human_matches_git() {
    // `human` hides the fields the reader can infer from the current clock, so
    // what it prints depends on how far the commit is from now — which is why
    // it is checked against git rather than against a literal. Both read the
    // same clock on the same fixed commit date, so the rendering is identical.
    let (repo, home) = dated_fixture("human", "1700000000 +0000");

    let z = zvcs_blame(&repo, &home, &["--date=human"]);
    let g = real_blame(&repo, &home, &["--date=human"]);
    assert!(
        z.status.success(),
        "zvcs --date=human failed: {}",
        String::from_utf8_lossy(&z.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&g.stdout),
        stdout(&z),
        "--date=human must match git"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn blame_date_relative_matches_git() {
    // `relative` renders against the current wall clock; git and zvcs read the
    // same clock microseconds apart on a fixed commit date, so the coarse bucket
    // ("N years[, M months] ago") is identical.
    let (repo, home) = dated_fixture("relative", "1700000000 +0000");

    let z = zvcs_blame(&repo, &home, &["--date=relative"]);
    let g = real_blame(&repo, &home, &["--date=relative"]);
    assert!(
        z.status.success(),
        "zvcs --date=relative failed: {}",
        String::from_utf8_lossy(&z.stderr)
    );
    let zs = stdout(&z);
    assert!(zs.contains(" ago"), "relative renders an 'ago' phrase:\n{zs}");
    assert_eq!(
        String::from_utf8_lossy(&g.stdout),
        zs,
        "--date=relative must match git"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// The `-local` conversion itself, which the test above cannot catch: its
/// helper pins `TZ=UTC`, where `iso-local` and `iso` agree even if `-local`
/// were a no-op. Here the zone is the thing under test, so a port that ignored
/// it would fail on two of the three rows.
///
/// POSIX `TZ` strings rather than zoneinfo names, so this holds on a headless
/// runner with no tzdata installed. `JST-9` is UTC+9 and `EST5` is UTC-5 —
/// both DST-free, so the expected renderings are fixed. The values are stock
/// git's, captured byte-for-byte on this fixture's timestamp; `date.c`'s
/// `show_date()` under `DATE_ISO8601|local` converts with `localtime_r()` and
/// keeps the zone field, which is why the offset moves with `TZ`.
#[test]
fn blame_date_local_converts_to_the_ambient_zone() {
    let (repo, home) = dated_fixture("tzlocal", "1700000000 +0000");

    for (tz, want) in [
        ("JST-9", "2023-11-15 07:13:20 +0900"),
        ("EST5", "2023-11-14 17:13:20 -0500"),
        ("UTC", "2023-11-14 22:13:20 +0000"),
    ] {
        let out = Command::new(BIN)
            .args(["blame", "--date=iso-local", "f"])
            .current_dir(&repo)
            .env("HOME", &home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("ZVCS_HOME", &home)
            .env("LC_ALL", "C")
            .env("TZ", tz)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "TZ={tz} --date=iso-local failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let got = String::from_utf8_lossy(&out.stdout);
        assert!(got.contains(want), "TZ={tz}: expected {want} in:\n{got}");
    }

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
