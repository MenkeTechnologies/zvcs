//! Config-driven `git help` behavior. Two keys reach this command:
//!   * `help.autocorrect` — parsed in the unknown-verb path (see the
//!     `autocorrect` integration test); not re-tested here.
//!   * `help.format` — read by `git help <topic>` to pick a viewer. Only the
//!     plain `man` viewer is implemented, so a non-`man` `help.format` (and,
//!     symmetrically, a non-`man` `man.viewer`) is a faithful-unsupported gate
//!     that must fire before `man` is ever spawned.
//!
//! `help.htmlPath` / `help.browser` are intentionally NOT read: both only steer
//! the web format (`git help -w`), which this port rejects outright, so reading
//! them would fabricate behavior with no code path behind it.
//!
//! The one place `help.format` is git-parity rather than a divergence is the
//! alias path: an alias resolves and prints its expansion BEFORE the viewer gate
//! is consulted, exactly as stock git does — that case is asserted byte-for-byte
//! against the installed git.

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
    let root = std::env::temp_dir().join(format!("zvcs-help-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    (repo, home)
}

/// Run the zvcs binary with stdin closed so a viewer can never block the test.
fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

/// An alias resolves and prints its expansion before `help.format` is consulted,
/// so `git help <alias>` succeeds even under an unsupported viewer — matching
/// stock git byte-for-byte on stdout and exit code.
#[test]
fn alias_expansion_precedes_format_gate() {
    let (repo, home) = fixture("alias");
    git(&repo, &["config", "alias.co", "checkout"]);
    git(&repo, &["config", "help.format", "html"]);

    let out = run(&repo, &home, &["help", "co"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "'co' is aliased to 'checkout'\n");

    // Byte-for-byte against the installed git: it resolves the alias ahead of the
    // viewer selection too.
    let real = Command::new("git")
        .args(["-c", "help.format=html", "help", "co"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert_eq!(out.stdout, real.stdout, "alias line must match stock git");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// A non-`man` `help.format` on a real topic fails through the viewer gate — and
/// crucially fails BEFORE `man` is spawned, so the value is genuinely honored
/// rather than ignored. The error names the offending value.
#[test]
fn unsupported_help_format_is_rejected() {
    let (repo, home) = fixture("format");
    git(&repo, &["config", "help.format", "html"]);

    let out = run(&repo, &home, &["help", "status"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("help.format=html"), "stderr:\n{stderr}");
    assert!(stderr.contains("only the plain `man` viewer is"), "stderr:\n{stderr}");
    // The gate fired ahead of any viewer, so nothing reached stdout.
    assert!(String::from_utf8_lossy(&out.stdout).is_empty());

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// The symmetric gate: a non-`man` `man.viewer` is rejected the same way, naming
/// the configured viewer.
#[test]
fn unsupported_man_viewer_is_rejected() {
    let (repo, home) = fixture("viewer");
    git(&repo, &["config", "man.viewer", "konqueror"]);

    let out = run(&repo, &home, &["help", "status"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("man.viewer=konqueror"), "stderr:\n{stderr}");
    assert!(stderr.contains("only the plain `man` viewer is"), "stderr:\n{stderr}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `git help --config` prints git's configuration-variable-name list: one name
/// per line, ASCII-sorted and unique, then a blank line and the
/// `'git help config' for more information` trailer, exit 0. The exact set is
/// pinned to git 2.55.0 in [`CONFIG_VARS`], so this asserts the structure and a
/// few stable anchors rather than a byte diff against whatever git the CI host
/// carries (the variable set drifts across git versions; this shape does not).
#[test]
fn config_lists_variable_names() {
    let (repo, home) = fixture("config");

    let out = run(&repo, &home, &["help", "--config"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8(out.stdout).unwrap();

    // Structure: <names>\n\n<trailer>\n.
    let trailer = "\n\n'git help config' for more information\n";
    assert!(stdout.ends_with(trailer), "missing trailer:\n{stdout}");
    let names: Vec<&str> = stdout[..stdout.len() - trailer.len()].lines().collect();

    // Sorted, unique, non-empty — matching git's `string_list_sort` output.
    assert!(!names.is_empty());
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "names must be ASCII-sorted");
    let unique: std::collections::BTreeSet<&str> = names.iter().copied().collect();
    assert_eq!(unique.len(), names.len(), "names must be unique");

    // Anchors that have existed for many git releases, including a wildcard and a
    // placeholder form git emits verbatim.
    for anchor in ["user.name", "core.editor", "alias.*", "branch.<name>.remote"] {
        assert!(names.contains(&anchor), "expected {anchor:?} in list");
    }

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// The short spelling `-c` is the same cmdmode as `--config` and produces the
/// identical listing.
#[test]
fn config_short_flag_matches_long() {
    let (repo, home) = fixture("config-short");

    let long = run(&repo, &home, &["help", "--config"]);
    let short = run(&repo, &home, &["help", "-c"]);
    assert!(short.status.success());
    assert_eq!(short.stdout, long.stdout, "-c must match --config byte-for-byte");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
