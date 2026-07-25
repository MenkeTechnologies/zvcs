//! `git whatchanged` shares `git log`'s `log.*` display config. `whatchanged` has its
//! own porcelain (it does not delegate to `log`), so these are regression guards that
//! its own config path honors `log.abbrevCommit`, `log.showRoot` and `log.date` the same
//! way — config supplies the default, the command line overrides, and an invalid
//! `log.date` is fatal (exit 128) at config read, ahead of the deprecation gate.
//!
//! Every assertion is verified byte-for-byte against the real `git` on `PATH` (2.55.0):
//! the zvcs stdout must equal real git's stdout for the same invocation. `whatchanged`
//! is deprecated in modern git and refuses to run without `--i-still-use-this`, which is
//! passed throughout; the deprecation notice modern git prints goes to stderr, so stdout
//! stays comparable.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A STOCK git to compare against, or `None` when the machine has no foreign git
/// installed.
///
/// These are differential tests: their whole point is to diff zvcs against
/// another implementation, so they are the one place a foreign binary is
/// legitimate. It is resolved EXPLICITLY (`ZVCS_STOCK_GIT`, else the system
/// path) rather than through `PATH`, because on a machine where zvcs shadows
/// git — the machine this is developed on — `PATH` resolution silently makes the
/// oracle the thing under test, and the comparison proves nothing. When no stock
/// git exists the oracle half is skipped and the zvcs-side assertions still run.
fn stock_git() -> Option<String> {
    if let Ok(p) = std::env::var("ZVCS_STOCK_GIT") {
        return std::path::Path::new(&p).exists().then_some(p);
    }
    ["/usr/bin/git", "/opt/homebrew/bin/git", "/usr/local/bin/git"]
        .into_iter()
        .find(|p| std::path::Path::new(p).exists())
        .map(str::to_owned)
}

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new(BIN).args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

/// A two-commit repo (root `c0`, child `c1`) with a fixed author/committer date so every
/// rendered `Date:` line and object id is deterministic across git and zvcs.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-wccfg-{tag}-{}", std::process::id()));
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
    git(&repo, &["config", "commit.gpgsign", "false"]);
    // 1136214245 = 2006-01-02T15:04:05Z — a single-digit day, which is exactly the case
    // git renders unpadded (`Jan 2`, not `Jan  2`); the fixture would expose a padding bug.
    std::fs::write(repo.join("f"), "hello\n").unwrap();
    git(&repo, &["add", "f"]);
    commit(&repo, "c0");
    std::fs::write(repo.join("f"), "hello\nworld\n").unwrap();
    git(&repo, &["add", "f"]);
    commit(&repo, "c1");
    (repo, home)
}

fn commit(repo: &Path, msg: &str) {
    assert!(Command::new(BIN)
        .args(["commit", "-q", "-m", msg])
        .current_dir(repo)
        .env("GIT_AUTHOR_DATE", "1136214245 +0000")
        .env("GIT_COMMITTER_DATE", "1136214245 +0000")
        .status()
        .unwrap()
        .success());
}

fn config(repo: &Path, key: &str, val: &str) {
    git(repo, &["config", key, val]);
}
fn unset(repo: &Path, key: &str) {
    // `--unset` fails if the key is absent; tolerate that so tests can share a fixture.
    let _ = Command::new(BIN)
        .args(["config", "--unset", key])
        .current_dir(repo)
        .status();
}

/// Run a binary's `whatchanged` in `repo` with a hermetic environment and fixed dates.
fn run(bin: &str, repo: &Path, home: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["whatchanged", "--i-still-use-this"];
    args.extend_from_slice(extra);
    Command::new(bin)
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

fn zvcs(repo: &Path, home: &Path, extra: &[&str]) -> Output {
    run(BIN, repo, home, extra)
}
/// Whether the stock git found on this machine can even run the invocation these
/// tests diff against. `whatchanged` requires `--i-still-use-this` on the gits
/// that deprecated it and REJECTS the flag on older ones (Apple git 2.50.1 does),
/// so on such a machine the oracle half is skipped rather than reported as a zvcs
/// divergence — the comparison would be measuring the oracle's age.
fn oracle_usable(repo: &Path, home: &Path) -> bool {
    let out = real(repo, home, &[]);
    !String::from_utf8_lossy(&out.stderr).contains("unrecognized argument")
}

fn real(repo: &Path, home: &Path, extra: &[&str]) -> Output {
    run(&stock_git().unwrap_or_else(|| "/usr/bin/git".into()), repo, home, extra)
}

/// zvcs stdout and exit code must equal real git's for the same invocation.
fn assert_stdout_matches(repo: &Path, home: &Path, extra: &[&str]) {
    if !oracle_usable(repo, home) {
        return;
    }
    let r = real(repo, home, extra);
    let z = zvcs(repo, home, extra);
    assert_eq!(
        z.status.code(),
        r.status.code(),
        "exit code diverged for {extra:?}\nzvcs stderr: {}",
        String::from_utf8_lossy(&z.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&z.stdout),
        String::from_utf8_lossy(&r.stdout),
        "stdout diverged from real git for {extra:?}"
    );
}

#[test]
fn default_output_matches_real_git_byte_for_byte() {
    // Baseline: with no config, the whole medium+raw rendering must match real git,
    // including the unpadded single-digit day in the `Date:` line (`Jan 2`, not `Jan  2`).
    let (repo, home) = fixture("default");
    assert_stdout_matches(&repo, &home, &[]);
    let out = zvcs(&repo, &home, &[]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("Date:   Mon Jan 2 15:04:05 2006 +0000"), "unpadded day:\n{s}");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn log_abbrev_commit_config_and_override() {
    let (repo, home) = fixture("abbrev");
    let full = {
        let out =
            Command::new(BIN).args(["rev-parse", "HEAD"]).current_dir(&repo).output().unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };

    // Default: full 40-char id in the header, byte-identical to git.
    assert_stdout_matches(&repo, &home, &[]);
    assert!(String::from_utf8_lossy(&zvcs(&repo, &home, &[]).stdout).contains(&format!("commit {full}")));

    // log.abbrevCommit=true → abbreviated header; must equal git's abbreviation width.
    config(&repo, "log.abbrevCommit", "true");
    assert_stdout_matches(&repo, &home, &[]);
    let z = String::from_utf8_lossy(&zvcs(&repo, &home, &[]).stdout).into_owned();
    assert!(!z.contains(&format!("commit {full}")), "config should abbreviate:\n{z}");
    assert!(z.lines().next().unwrap().starts_with("commit "), "still a commit header:\n{z}");

    // --no-abbrev-commit overrides config back to the full id.
    assert_stdout_matches(&repo, &home, &["--no-abbrev-commit"]);
    assert!(String::from_utf8_lossy(&zvcs(&repo, &home, &["--no-abbrev-commit"]).stdout)
        .contains(&format!("commit {full}")));

    // --abbrev-commit with no config also abbreviates.
    unset(&repo, "log.abbrevCommit");
    assert_stdout_matches(&repo, &home, &["--abbrev-commit"]);
    assert!(!String::from_utf8_lossy(&zvcs(&repo, &home, &["--abbrev-commit"]).stdout)
        .contains(&format!("commit {full}")));

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn log_show_root_config_and_override() {
    let (repo, home) = fixture("showroot");

    // Default (showRoot=true): both commits (root c0 + child c1) appear, byte-identical.
    assert_stdout_matches(&repo, &home, &[]);
    assert_eq!(
        String::from_utf8_lossy(&zvcs(&repo, &home, &[]).stdout).matches("\ncommit ").count()
            + 1, // first header has no preceding newline
        2,
        "default shows both commits"
    );

    // log.showRoot=false: the root's empty-tree diff is suppressed, so the root commit
    // drops out entirely — only c1 remains. Must match git byte-for-byte.
    config(&repo, "log.showRoot", "false");
    assert_stdout_matches(&repo, &home, &[]);
    let z = String::from_utf8_lossy(&zvcs(&repo, &home, &[]).stdout).into_owned();
    assert!(z.contains("    c1"), "child commit still shown:\n{z}");
    assert!(!z.contains("    c0"), "root commit suppressed:\n{z}");

    // --root forces the root diff back on, overriding log.showRoot=false.
    assert_stdout_matches(&repo, &home, &["--root"]);
    assert!(String::from_utf8_lossy(&zvcs(&repo, &home, &["--root"]).stdout).contains("    c0"));

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn log_show_root_false_single_root_repo_is_empty() {
    // A repo whose only commit is the root: log.showRoot=false leaves nothing to show, so
    // whatchanged exits 0 with empty stdout, exactly like git.
    let root = std::env::temp_dir().join(format!("zvcs-wccfg-solo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "a@example.com"]);
    git(&repo, &["config", "user.name", "A"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("f"), "x\n").unwrap();
    git(&repo, &["add", "f"]);
    commit(&repo, "root");
    config(&repo, "log.showRoot", "false");

    let z = zvcs(&repo, &home, &[]);
    assert_eq!(z.status.code(), Some(0), "exit 0 with only a suppressed root");
    assert!(z.stdout.is_empty(), "empty stdout: {:?}", String::from_utf8_lossy(&z.stdout));
    assert_stdout_matches(&repo, &home, &[]);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn log_date_default_is_a_noop_matching_git() {
    // log.date=default renders exactly DATE_NORMAL — the same output whatchanged produces
    // with no config — so it stays byte-identical to git.
    let (repo, home) = fixture("date-default");
    config(&repo, "log.date", "default");
    assert_stdout_matches(&repo, &home, &[]);
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn log_date_invalid_is_fatal_before_deprecation_and_override() {
    let (repo, home) = fixture("date-bad");
    config(&repo, "log.date", "bogus");

    // git validates log.date in git_log_config, before setup_revisions and before the
    // deprecation gate: an invalid value is `fatal: unknown date format bogus` (exit 128),
    // stderr byte-identical to git. Verified with --i-still-use-this present.
    let r = real(&repo, &home, &[]);
    let z = zvcs(&repo, &home, &[]);
    assert_eq!(z.status.code(), Some(128), "invalid log.date must exit 128");
    assert_eq!(z.status.code(), r.status.code());
    assert_eq!(
        String::from_utf8_lossy(&z.stderr),
        String::from_utf8_lossy(&r.stderr),
        "stderr must match git's date-format fatal"
    );
    assert!(String::from_utf8_lossy(&z.stderr).contains("unknown date format bogus"));

    // The fatal fires even without --i-still-use-this: the date error precedes the
    // deprecation notice (both exit 128, but the message is the date error, not the notice).
    let z2 = Command::new(BIN)
        .args(["whatchanged"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", &home)
        .output()
        .unwrap();
    assert_eq!(z2.status.code(), Some(128));
    let e2 = String::from_utf8_lossy(&z2.stderr);
    assert!(e2.contains("unknown date format bogus"), "date fatal, not deprecation:\n{e2}");
    assert!(!e2.contains("nominated for removal"), "deprecation notice must not appear:\n{e2}");

    // Config validation also wins over a valid command-line --date override.
    let z3 = zvcs(&repo, &home, &["--date=unix"]);
    assert_eq!(z3.status.code(), Some(128));
    assert!(String::from_utf8_lossy(&z3.stderr).contains("unknown date format bogus"));

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn log_date_valid_nondefault_bails_but_empty_walk_exits_zero() {
    // whatchanged renders only DATE_NORMAL, so a valid non-default log.date (e.g. short)
    // selects a format it cannot produce. Rather than emit a wrong Date: line it bails when
    // a commit would be shown — identical treatment to a command-line --date.
    let (repo, home) = fixture("date-short");
    config(&repo, "log.date", "short");
    let z = zvcs(&repo, &home, &[]);
    assert_ne!(z.status.code(), Some(0), "must not silently emit a wrong Date line");
    assert!(
        String::from_utf8_lossy(&z.stderr).contains("log.date"),
        "bail names the unported config: {}",
        String::from_utf8_lossy(&z.stderr)
    );
    // Real git happily renders the short date here — this is the one documented
    // divergence, and it can only be checked where the oracle runs at all.
    if oracle_usable(&repo, &home) {
        assert!(String::from_utf8_lossy(&real(&repo, &home, &[]).stdout).contains("Date:   2006-01-02"));
    }

    // But when a filter empties the walk, the unported option is never applied, so both
    // git and zvcs exit 0 with empty output — the deferred bail must not fire early.
    assert_stdout_matches(&repo, &home, &["--grep=NOPExyz"]);
    let empty = zvcs(&repo, &home, &["--grep=NOPExyz"]);
    assert_eq!(empty.status.code(), Some(0));
    assert!(empty.stdout.is_empty());

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
