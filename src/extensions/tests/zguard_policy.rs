//! `git zguard` / `git zpolicy` — the command policy that refuses a command
//! *before it runs*.
//!
//! This verb had no integration test. That is a bad place for the gap: every
//! other superset verb reports something, and a report that regresses is
//! visible the next time somebody reads it, but a *gate* that stops refusing
//! is silent — the command it was supposed to block simply succeeds, and
//! nothing says so. The unit tests in `superset::guard` cover the matcher; what
//! was untested is the half that matters, which is whether the dispatcher
//! consults it at all.
//!
//! So these run the real binary and assert on the **side effect**, not on the
//! message: a denied `commit` must leave `HEAD` where it was, and a warned one
//! must move it. A gate that prints "blocked" and then runs the command passes
//! any test that only reads stdout.
//!
//! Every case is hermetic: `ZVCS_HOME` points into the fixture, so the
//! machine-wide `guards.tsv` is the fixture's own and the developer's real one
//! is neither read nor written.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home.join("zvcs"))
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
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

/// A repository with one commit, and an isolated `$ZVCS_HOME`.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-zguard-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(home.join("zvcs")).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    ok(&run(&repo, &home, &["init", "-q", "-b", "main", "."]), "init");
    std::fs::write(repo.join("a.txt"), b"one\n").unwrap();
    ok(&run(&repo, &home, &["add", "a.txt"]), "add");
    ok(&run(&repo, &home, &["commit", "-q", "-m", "first"]), "commit");
    (repo, home)
}

/// The commit `HEAD` names, so a test can prove a refused command changed
/// nothing.
fn head(repo: &Path, home: &Path) -> String {
    ok(&run(repo, home, &["rev-parse", "HEAD"]), "rev-parse").trim().to_string()
}

#[test]
fn an_empty_registry_leaves_no_file_and_says_so() {
    // The whole cost of this feature on a machine that does not use it is one
    // failed `stat`, which only holds while the registry file is *absent* —
    // `save` deletes it rather than writing an empty one. If that ever becomes
    // an empty file, every git command on every machine starts opening it.
    let (repo, home) = fixture("empty");
    let out = ok(&run(&repo, &home, &["zguard", "list"]), "zguard list");
    assert_eq!(out.trim(), "no guards");
    assert!(!home.join("zvcs/guards.tsv").exists(), "an empty registry wrote a file");

    // And with nothing registered, an ordinary command is untouched.
    let before = head(&repo, &home);
    std::fs::write(repo.join("a.txt"), b"two\n").unwrap();
    ok(&run(&repo, &home, &["commit", "-qam", "second"]), "commit");
    assert_ne!(head(&repo, &home), before);

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn a_deny_rule_stops_the_command_from_happening() {
    let (repo, home) = fixture("deny");
    let out = ok(&run(&repo, &home, &["zguard", "deny", "commit*"]), "zguard deny");
    assert!(out.contains("guard #1: deny `commit*`"), "{out}");
    assert!(home.join("zvcs/guards.tsv").exists());

    let before = head(&repo, &home);
    std::fs::write(repo.join("a.txt"), b"two\n").unwrap();
    let out = run(&repo, &home, &["commit", "-qam", "second"]);
    assert!(!out.status.success(), "a denied commit succeeded");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("blocked"),
        "no refusal on stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The point of the whole feature: the command did not run.
    assert_eq!(head(&repo, &home), before, "a denied commit still moved HEAD");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn a_warn_rule_reports_and_then_lets_it_through() {
    let (repo, home) = fixture("warn");
    ok(&run(&repo, &home, &["zguard", "warn", "commit*", "-m", "mind the branch"]), "zguard warn");

    let before = head(&repo, &home);
    std::fs::write(repo.join("a.txt"), b"two\n").unwrap();
    let out = run(&repo, &home, &["commit", "-qam", "second"]);
    assert!(out.status.success(), "a warned commit was refused");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("warning"), "no warning on stderr: {err}");
    assert!(err.contains("mind the branch"), "the custom message is missing: {err}");
    // Warned, not blocked.
    assert_ne!(head(&repo, &home), before, "a warned commit did not run");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn a_predicate_decides_whether_the_rule_applies() {
    // `--when protected` is true on `main` and false on any other branch, so
    // the same rule has to refuse one and allow the other. A port that ignores
    // the predicate refuses both, and one that mis-evaluates it refuses
    // neither; the pair separates all three.
    let (repo, home) = fixture("pred");
    ok(&run(&repo, &home, &["zguard", "deny", "commit*", "--when", "protected"]), "zguard deny");

    std::fs::write(repo.join("a.txt"), b"two\n").unwrap();
    let on_main = run(&repo, &home, &["commit", "-qam", "on main"]);
    assert!(!on_main.status.success(), "the rule did not fire on the protected branch");

    ok(&run(&repo, &home, &["checkout", "-q", "-b", "side"]), "checkout -b");
    let before = head(&repo, &home);
    let on_side = run(&repo, &home, &["commit", "-qam", "on side"]);
    assert!(
        on_side.status.success(),
        "the rule fired off the protected branch: {}",
        String::from_utf8_lossy(&on_side.stderr)
    );
    assert_ne!(head(&repo, &home), before);

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn the_policy_verbs_are_exempt_from_the_policy() {
    // The lockout guarantee. `deny '*'` matches every command, including the
    // ones that manage the rules — so without the exemption in `dispatch::run`
    // a single rule would make the registry unreachable and the only way out
    // would be to delete a file by hand.
    let (repo, home) = fixture("exempt");
    ok(&run(&repo, &home, &["zguard", "deny", "*", "-m", "everything is blocked"]), "zguard deny *");

    // An ordinary command is now refused …
    let out = run(&repo, &home, &["status", "--porcelain"]);
    assert!(!out.status.success(), "`deny *` did not refuse an ordinary command");

    // … while both spellings of the policy verb still answer.
    let listed = ok(&run(&repo, &home, &["zguard", "list"]), "zguard list under deny *");
    assert!(listed.contains("deny"), "{listed}");
    let listed = ok(&run(&repo, &home, &["zpolicy", "list"]), "zpolicy list under deny *");
    assert!(listed.contains("deny"), "{listed}");

    // And clearing works, which is the way out.
    ok(&run(&repo, &home, &["zguard", "clear"]), "zguard clear");
    assert!(!home.join("zvcs/guards.tsv").exists(), "clear left the registry file behind");
    ok(&run(&repo, &home, &["status", "--porcelain"]), "status after clear");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn test_reports_the_verdict_without_running_anything() {
    // `zguard test` is the dry run: it has to reach the same verdict the
    // dispatcher would, and exit non-zero on a deny so a script can gate on it.
    let (repo, home) = fixture("dry");
    ok(&run(&repo, &home, &["zguard", "deny", "push*--force*"]), "deny force-push");
    ok(&run(&repo, &home, &["zguard", "warn", "rm*"]), "warn rm");

    let denied = run(&repo, &home, &["zguard", "test", "push", "--force", "origin", "main"]);
    assert!(!denied.status.success(), "a denied command tested as allowed");
    assert!(String::from_utf8_lossy(&denied.stdout).starts_with("DENY"), "{denied:?}");

    let warned = ok(&run(&repo, &home, &["zguard", "test", "rm", "a.txt"]), "test rm");
    assert!(warned.starts_with("WARN"), "{warned}");

    let allowed = ok(&run(&repo, &home, &["zguard", "test", "status"]), "test status");
    assert_eq!(allowed.trim(), "allow");

    // The dry run is a *read*: the working tree it named is untouched.
    assert!(repo.join("a.txt").exists(), "`zguard test rm` removed the file");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn rules_are_removed_one_at_a_time_by_id() {
    let (repo, home) = fixture("rm");
    ok(&run(&repo, &home, &["zguard", "deny", "commit*"]), "rule 1");
    ok(&run(&repo, &home, &["zguard", "warn", "push*"]), "rule 2");
    let listed = ok(&run(&repo, &home, &["zguard", "list"]), "list");
    assert!(listed.contains("#1") && listed.contains("#2"), "{listed}");

    let out = ok(&run(&repo, &home, &["zguard", "rm", "1"]), "rm 1");
    assert!(out.contains("removed guard #1"), "{out}");
    let listed = ok(&run(&repo, &home, &["zguard", "list"]), "list after rm");
    assert!(!listed.contains("#1") && listed.contains("#2"), "{listed}");

    // The commit rule is gone, so committing works again.
    std::fs::write(repo.join("a.txt"), b"two\n").unwrap();
    ok(&run(&repo, &home, &["commit", "-qam", "second"]), "commit after rm");

    // Removing an id that is not there says so and changes nothing.
    let out = ok(&run(&repo, &home, &["zguard", "rm", "99"]), "rm 99");
    assert!(out.contains("no guard #99"), "{out}");
    assert!(ok(&run(&repo, &home, &["zguard", "list"]), "list").contains("#2"));

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
