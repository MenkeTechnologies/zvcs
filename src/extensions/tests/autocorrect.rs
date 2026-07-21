//! `help.autocorrect` — the entry point must reproduce git's `help_unknown_cmd`
//! for an unknown verb: rank commands by edit distance and, per the config
//! value, auto-run the nearest one or print git's "not a git command" with the
//! closest suggestions. Regression guard for unknown verbs bypassing autocorrect
//! (they died as "not yet ported").

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

/// A hermetic repo with an isolated HOME so config reads only what we set.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-ac-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["-c", "user.email=t@e.x", "-c", "user.name=t", "commit", "--allow-empty", "-q", "-m", "first"]);
    (repo, home)
}

fn run(dir: &Path, home: &Path, autocorrect: Option<&str>, args: &[&str]) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .stdin(std::process::Stdio::null());
    if let Some(v) = autocorrect {
        git(dir, &["config", "help.autocorrect", v]);
    }
    cmd.output().unwrap()
}

/// `help.autocorrect=1` (immediate): the WARNING is printed and the nearest
/// command actually runs. `verison` → `version`, which prints the version.
#[test]
fn immediate_runs_nearest_command() {
    let (repo, home) = fixture("immediate");
    let out = run(&repo, &home, Some("1"), &["verison"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("which does not exist.") && stderr.contains("you meant 'version'"),
        "stderr:\n{stderr}"
    );
    // The corrected command ran and produced its output on stdout.
    assert!(
        String::from_utf8_lossy(&out.stdout).starts_with("git version "),
        "corrected command should have run; stdout:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `help.autocorrect=show` prints git's diagnostic plus the nearest suggestion
/// and does NOT run anything.
#[test]
fn show_lists_suggestions_without_running() {
    let (repo, home) = fixture("show");
    let out = run(&repo, &home, Some("show"), &["whatchnged"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("is not a git command"), "stderr:\n{stderr}");
    assert!(stderr.contains("The most similar command"), "stderr:\n{stderr}");
    assert!(stderr.contains("whatchanged"), "stderr:\n{stderr}");
    // Nothing ran: no log output.
    assert!(String::from_utf8_lossy(&out.stdout).is_empty());
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `help.autocorrect=never` prints only the terse diagnostic — no suggestions.
#[test]
fn never_is_terse() {
    let (repo, home) = fixture("never");
    let out = run(&repo, &home, Some("never"), &["whatchnged"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("is not a git command"), "stderr:\n{stderr}");
    assert!(!stderr.contains("The most similar"), "never must not suggest:\n{stderr}");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// A real verb is never routed through autocorrect, even with a same-distance
/// typo-ish name — `status` dispatches normally.
#[test]
fn known_verb_is_not_autocorrected() {
    let (repo, home) = fixture("known");
    let out = run(&repo, &home, Some("1"), &["status"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(!String::from_utf8_lossy(&out.stderr).contains("does not exist"));
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
