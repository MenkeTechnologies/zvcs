//! `git grep` honors its config defaults — `grep.lineNumber` and
//! `grep.patternType` — with CLI flags still overriding. Regression guard for
//! these being ignored (hardcoded `-n` off, basic dialect).

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
    let mut args = vec!["grep"];
    args.extend_from_slice(extra);
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
