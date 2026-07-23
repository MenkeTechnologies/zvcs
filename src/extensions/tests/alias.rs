//! gitconfig `alias.<cmd>` expansion. The entry point must resolve aliases the
//! way git's `run_argv` does: a real verb wins over a same-named alias, a
//! git-command alias expands and re-dispatches, a `!`-shell alias runs via the
//! shell with the user's args as `"$@"`, and self / looping references are
//! rejected with git's diagnostics. Regression guard for aliases being ignored
//! entirely (every `alias.<cmd>` died as "not yet ported").

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

/// A hermetic repo with an isolated HOME, so alias lookup reads only the config
/// we set — never the developer's real global gitconfig.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-alias-{tag}-{}", std::process::id()));
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

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .output()
        .unwrap()
}

#[test]
fn expands_git_command_alias() {
    let (repo, home) = fixture("cmd");
    git(&repo, &["config", "alias.zzlast", "log -1 --format=%s"]);

    let out = run(&repo, &home, &["zzlast"]);
    assert!(out.status.success(), "zzlast failed: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "first");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn alias_passes_through_user_arguments() {
    let (repo, home) = fixture("args");
    git(&repo, &["config", "alias.zzshow", "log --format=%s"]);

    // The `-1` the user appends must survive splicing after the expansion.
    let out = run(&repo, &home, &["zzshow", "-1"]);
    assert!(out.status.success(), "zzshow -1 failed: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "first");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn builtin_wins_over_same_named_alias() {
    let (repo, home) = fixture("precedence");
    // `version` is a real verb; an alias of the same name must be ignored.
    git(&repo, &["config", "alias.version", "log -1"]);

    let out = run(&repo, &home, &["version"]);
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).starts_with("git version "),
        "builtin `version` should win over alias.version"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn runs_shell_alias_with_args() {
    let (repo, home) = fixture("shell");
    git(&repo, &["config", "alias.zzecho", "!echo shelled $1"]);

    let out = run(&repo, &home, &["zzecho", "hi"]);
    assert!(out.status.success(), "shell alias failed: {}", String::from_utf8_lossy(&out.stderr));
    // git binds `$1` and also appends the args as `"$@"`, so "hi" appears twice.
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "shelled hi hi");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn rejects_recursive_alias() {
    let (repo, home) = fixture("recursive");
    git(&repo, &["config", "alias.zzr", "zzr x"]);

    let out = run(&repo, &home, &["zzr"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("recursive alias: zzr"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn rejects_alias_loop() {
    let (repo, home) = fixture("loop");
    git(&repo, &["config", "alias.zza", "zzb"]);
    git(&repo, &["config", "alias.zzb", "zza"]);

    let out = run(&repo, &home, &["zza"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("alias loop detected"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
