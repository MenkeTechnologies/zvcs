//! `git grep` honors its config defaults — `grep.lineNumber`,
//! `grep.patternType`, `grep.column`, `grep.extendedRegexp` and
//! `grep.fullName` — with CLI flags still overriding. Regression guard for
//! these being ignored (hardcoded `-n` off, no column, basic dialect,
//! relative names).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-grepcfg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@e.x"]);
    git(&repo, &["config", "user.name", "t"]);
    // "a.b" and "axb": the dot discriminates fixed (literal) from basic (wildcard).
    std::fs::write(repo.join("f"), "a.b\naxb\n").unwrap();
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-q", "-m", "c0"]);
    (repo, home)
}

fn grep(repo: &Path, home: &Path, extra: &[&str]) -> Output {
    grep_in(repo, home, extra)
}

/// Run zvcs `grep` with `cwd` as the working directory (which is `repo` itself
/// for most tests, but a subdirectory for `--full-name`, whose whole point is to
/// print repo-root-relative paths from a subtree).
fn grep_in(cwd: &Path, home: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["grep"];
    args.extend_from_slice(extra);
    Command::new(BIN)
        .args(&args)
        .current_dir(cwd)
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
fn grep_line_number_config_and_override() {
    let (repo, home) = fixture("linenum");
    git(&repo, &["config", "grep.lineNumber", "true"]);
    // Default -n from config: output carries the line number.
    let d = stdout(&grep(&repo, &home, &["axb", "--", "f"]));
    assert_eq!(d.trim(), "f:2:axb", "grep.lineNumber should add the line number:\n{d}");
    // --no-line-number overrides the config back off.
    let d = stdout(&grep(&repo, &home, &["--no-line-number", "axb", "--", "f"]));
    assert_eq!(d.trim(), "f:axb", "--no-line-number must override config:\n{d}");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn grep_pattern_type_config_and_override() {
    let (repo, home) = fixture("pattern");

    // Basic (git default): the dot is a wildcard → matches both "a.b" and "axb".
    let basic = stdout(&grep(&repo, &home, &["-c", "a.b", "--", "f"]));
    assert_eq!(basic.trim(), "f:2", "basic dialect: dot is a wildcard:\n{basic}");

    // grep.patternType=fixed → the dot is literal → only "a.b" matches.
    git(&repo, &["config", "grep.patternType", "fixed"]);
    let fixed = stdout(&grep(&repo, &home, &["-c", "a.b", "--", "f"]));
    assert_eq!(fixed.trim(), "f:1", "grep.patternType=fixed: dot is literal:\n{fixed}");

    // -E (extended) overrides the config back to a wildcard dot → both match.
    let ere = stdout(&grep(&repo, &home, &["-E", "-c", "a.b", "--", "f"]));
    assert_eq!(ere.trim(), "f:2", "-E must override grep.patternType:\n{ere}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn grep_column_config_and_override() {
    let (repo, home) = fixture("column");

    // No column by default: git prints only `<name>:<line>`.
    let plain = stdout(&grep(&repo, &home, &["axb", "--", "f"]));
    assert_eq!(plain.trim(), "f:axb", "default output carries no column:\n{plain}");

    // grep.column=true adds the 1-based match column, as `--column` would.
    git(&repo, &["config", "grep.column", "true"]);
    let col = stdout(&grep(&repo, &home, &["axb", "--", "f"]));
    assert_eq!(col.trim(), "f:1:axb", "grep.column should add the column:\n{col}");

    // --no-column overrides the config back off.
    let off = stdout(&grep(&repo, &home, &["--no-column", "axb", "--", "f"]));
    assert_eq!(off.trim(), "f:axb", "--no-column must override config:\n{off}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn grep_extended_regexp_config_and_override() {
    let (repo, home) = fixture("extended");

    // Basic (git default): `+` is a literal, so `a+` matches no line of "a.b\naxb".
    let basic = grep(&repo, &home, &["-c", "a+", "--", "f"]);
    assert_eq!(basic.status.code(), Some(1), "BRE a+ finds nothing (exit 1)");
    assert_eq!(stdout(&basic).trim(), "", "BRE a+: `+` is literal, no match:\n{}", stdout(&basic));

    // grep.extendedRegexp=true → ERE, `a+` is one-or-more `a` → both lines match.
    git(&repo, &["config", "grep.extendedRegexp", "true"]);
    let ere = grep(&repo, &home, &["-c", "a+", "--", "f"]);
    assert_eq!(stdout(&ere).trim(), "f:2", "grep.extendedRegexp: `+` is a quantifier:\n{}", stdout(&ere));

    // -G (basic) overrides the config back to a literal `+` → no match again.
    let g = grep(&repo, &home, &["-G", "-c", "a+", "--", "f"]);
    assert_eq!(g.status.code(), Some(1), "-G must override grep.extendedRegexp");
    assert_eq!(stdout(&g).trim(), "", "-G forces literal `+`, no match:\n{}", stdout(&g));

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn grep_full_name_config_and_override() {
    let (repo, home) = fixture("fullname");
    // A tracked file in a subdirectory: `--full-name`'s effect is only visible
    // when grep runs from a subtree, where the printed path is otherwise relative.
    let sub = repo.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("g"), "axb\n").unwrap();
    git(&repo, &["add", "sub/g"]);
    git(&repo, &["commit", "-q", "-m", "c1"]);

    // From the subdir, git prints the name relative to the current directory.
    let rel = stdout(&grep_in(&sub, &home, &["axb", "--", "g"]));
    assert_eq!(rel.trim(), "g:axb", "default name is cwd-relative:\n{rel}");

    // grep.fullName=true prints the repo-root-relative path instead.
    git(&repo, &["config", "grep.fullName", "true"]);
    let full = stdout(&grep_in(&sub, &home, &["axb", "--", "g"]));
    assert_eq!(full.trim(), "sub/g:axb", "grep.fullName should print the full path:\n{full}");

    // --no-full-name overrides the config back to the relative name.
    let off = stdout(&grep_in(&sub, &home, &["--no-full-name", "axb", "--", "g"]));
    assert_eq!(off.trim(), "g:axb", "--no-full-name must override config:\n{off}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
