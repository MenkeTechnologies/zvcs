//! `git zintercept` — aspect-oriented advice around any git subcommand.
//!
//! The third gate in this suite, and the last one that had unit tests for its
//! matcher and nothing for its effect. `superset::intercepts` covers
//! `intercept_matches` (exact / glob / `all`); untested was everything the
//! feature exists for, all of which lives in `dispatch::run`:
//!
//!  * **before** advice runs and the command *still runs*;
//!  * **around** advice *replaces* the command — so a rule that does not run
//!    `$INTERCEPT_CMD` must leave the repository untouched, which is the only
//!    assertion that separates "replaced" from "ran advice and then the
//!    command anyway";
//!  * an around advice that *does* run `$INTERCEPT_CMD` proceeds, and the
//!    child it spawns is **not intercepted again** — without the
//!    `ZVCS_INTERCEPTED` guard that is an unbounded recursion, and a test that
//!    only reads stdout would see a hang rather than a wrong answer;
//!  * **after** advice sees the command's exit status.
//!
//! Advice is a shell command, so every case here has it write a file: the file
//! is the evidence that advice ran, and its absence the evidence that it did
//! not. Ordering against the command is read from repository state (did the
//! commit happen) rather than from interleaved stdout, which is not ordered
//! across two processes.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    // `$INTERCEPT_CMD` is a `git …` line resolved through PATH, which is the
    // shadow-binary design: in production `git` *is* this binary. The fixture
    // reproduces that with a symlink, or an around advice would proceed into
    // whatever git the developer happens to have installed.
    let path = format!(
        "{}:{}",
        home.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("PATH", path)
        .env("HOME", home)
        .env("ZVCS_HOME", home.join("zvcs"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .env_remove("ZVCS_INTERCEPTED")
        .output()
        .unwrap()
}

fn ok(out: &Output, what: &str) -> String {
    assert!(
        out.status.success(),
        "{what} failed ({}): {}{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-zint-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    std::fs::create_dir_all(home.join("zvcs")).unwrap();
    std::fs::create_dir_all(home.join("bin")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(BIN, home.join("bin/git")).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    ok(&run(&repo, &home, &["init", "-q", "-b", "main", "."]), "init");
    std::fs::write(repo.join("a.txt"), b"one\n").unwrap();
    ok(&run(&repo, &home, &["add", "a.txt"]), "add");
    ok(&run(&repo, &home, &["commit", "-q", "-m", "first"]), "commit");
    (repo.canonicalize().unwrap(), home)
}

fn head(repo: &Path, home: &Path) -> String {
    ok(&run(repo, home, &["rev-parse", "HEAD"]), "rev-parse").trim().to_string()
}

/// The file advice writes into, read back as lines.
fn marks(home: &Path) -> Vec<String> {
    std::fs::read_to_string(home.join("marks"))
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// Shell that appends one word to the mark file.
fn mark_cmd(home: &Path, word: &str) -> String {
    format!("echo {word} >> {}", home.join("marks").display())
}

#[test]
fn with_no_rule_registered_nothing_runs() {
    let (repo, home) = fixture("none");
    assert!(!home.join("zvcs/intercepts.tsv").exists(), "a registry existed before any rule");
    let out = ok(&run(&repo, &home, &["zintercept", "list"]), "list");
    assert!(out.contains("no intercepts") || out.trim().is_empty(), "{out}");
    ok(&run(&repo, &home, &["status", "--porcelain"]), "status");
    assert!(marks(&home).is_empty());
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn before_advice_runs_and_the_command_still_happens() {
    let (repo, home) = fixture("before");
    let cmd = mark_cmd(&home, "before");
    ok(&run(&repo, &home, &["zintercept", "before", "commit*", "--", &cmd]), "register before");

    let start = head(&repo, &home);
    std::fs::write(repo.join("a.txt"), b"two\n").unwrap();
    ok(&run(&repo, &home, &["commit", "-qam", "second"]), "commit under before-advice");

    assert_eq!(marks(&home), vec!["before".to_string()], "before advice did not run exactly once");
    assert_ne!(head(&repo, &home), start, "before advice swallowed the command");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn around_advice_replaces_the_command() {
    // The property that makes `around` different from `before`: if the advice
    // does not run `$INTERCEPT_CMD`, the command must not happen at all.
    let (repo, home) = fixture("around-block");
    let cmd = mark_cmd(&home, "around");
    ok(&run(&repo, &home, &["zintercept", "around", "commit*", "--", &cmd]), "register around");

    let start = head(&repo, &home);
    std::fs::write(repo.join("a.txt"), b"two\n").unwrap();
    run(&repo, &home, &["commit", "-qam", "second"]);

    assert_eq!(marks(&home), vec!["around".to_string()], "around advice did not run");
    assert_eq!(head(&repo, &home), start, "around advice ran and the command happened anyway");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn around_advice_proceeds_through_intercept_cmd_without_recursing() {
    // Running `$INTERCEPT_CMD` is how an around advice lets the command
    // through. The child carries `ZVCS_INTERCEPTED`, so the same rule must not
    // fire for it — without that guard this recurses until something breaks,
    // and the mark file is what counts the firings.
    let (repo, home) = fixture("around-proceed");
    // `eval` is required: INTERCEPT_CMD is a whole command line, so the quoted
    // form without it makes `sh` look for one program named `git commit -qam
    // second` and answer `command not found` — which is how the documented
    // spelling turned every around advice into a silent block. Measured, and
    // the documentation now says `eval` too.
    let advice = format!("{}; eval \"$INTERCEPT_CMD\"", mark_cmd(&home, "around"));
    ok(&run(&repo, &home, &["zintercept", "around", "commit*", "--", &advice]), "register around");

    let start = head(&repo, &home);
    std::fs::write(repo.join("a.txt"), b"two\n").unwrap();
    let out = run(&repo, &home, &["commit", "-qam", "second"]);
    assert!(out.status.success(), "proceeding advice failed: {}", String::from_utf8_lossy(&out.stderr));

    assert_ne!(head(&repo, &home), start, "the command did not run through $INTERCEPT_CMD");
    assert_eq!(marks(&home), vec!["around".to_string()], "the advice fired more than once — the re-entry guard is gone");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn after_advice_sees_the_commands_exit_status() {
    // `after` is the only kind that runs the command itself in order to report
    // on it, so INTERCEPT_STATUS is its whole contract. A failing command must
    // report a non-zero status, a succeeding one zero.
    let (repo, home) = fixture("after");
    let marks_path = home.join("marks");
    let advice = format!("echo \"status=$INTERCEPT_STATUS name=$INTERCEPT_NAME\" >> {}", marks_path.display());
    ok(&run(&repo, &home, &["zintercept", "after", "commit*", "--", &advice]), "register after");

    // A commit with nothing staged fails.
    run(&repo, &home, &["commit", "-qm", "empty"]);
    let after_failure = marks(&home);
    assert_eq!(after_failure.len(), 1, "after advice did not run once: {after_failure:?}");
    assert!(after_failure[0].contains("name=commit"), "INTERCEPT_NAME missing: {after_failure:?}");
    assert!(
        !after_failure[0].contains("status=0"),
        "a failed command reported success to after-advice: {after_failure:?}"
    );

    // And one that succeeds reports zero.
    std::fs::write(repo.join("a.txt"), b"two\n").unwrap();
    run(&repo, &home, &["commit", "-qam", "second"]);
    let both = marks(&home);
    assert_eq!(both.len(), 2, "after advice did not run for the second command: {both:?}");
    assert!(both[1].contains("status=0"), "a successful command reported failure: {both:?}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn a_rule_only_fires_for_the_commands_it_names() {
    let (repo, home) = fixture("match");
    ok(
        &run(&repo, &home, &["zintercept", "before", "status*", "--", &mark_cmd(&home, "status")]),
        "register",
    );
    ok(&run(&repo, &home, &["log", "--oneline", "-1"]), "log");
    assert!(marks(&home).is_empty(), "a status rule fired for log");
    ok(&run(&repo, &home, &["status", "--porcelain"]), "status");
    assert_eq!(marks(&home), vec!["status".to_string()], "the status rule did not fire");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn rules_are_listed_removed_and_cleared() {
    let (repo, home) = fixture("manage");
    ok(&run(&repo, &home, &["zintercept", "before", "commit*", "--", "true"]), "rule 1");
    ok(&run(&repo, &home, &["zintercept", "after", "push*", "--", "true"]), "rule 2");
    let listed = ok(&run(&repo, &home, &["zintercept", "list"]), "list");
    assert!(listed.contains("commit*") && listed.contains("push*"), "{listed}");

    ok(&run(&repo, &home, &["zintercept", "remove", "1"]), "remove 1");
    let listed = ok(&run(&repo, &home, &["zintercept", "list"]), "list after remove");
    assert!(!listed.contains("commit*") && listed.contains("push*"), "{listed}");

    // Removing an id that is not there is an error rather than a silent no-op.
    let missing = run(&repo, &home, &["zintercept", "remove", "99"]);
    assert!(!missing.status.success(), "removing a missing id succeeded");

    ok(&run(&repo, &home, &["zintercept", "clear"]), "clear");
    // Cleared means the hot path is a failed stat again, as with `zguard`.
    assert!(!home.join("zvcs/intercepts.tsv").exists(), "clear left the registry file behind");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
