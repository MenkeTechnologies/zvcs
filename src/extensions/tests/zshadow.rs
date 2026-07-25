//! `git zshadow` is the one-command shadow install, and its stdout is meant to
//! be `eval`ed — two properties that break silently.
//!
//! The install must produce every piece the rest of the tree assumes exists
//! (the `git` shim, the dashed links, the man pages, the zsh `_git`), and stdout
//! must stay shell code alone: a stray summary line on stdout would be executed
//! by `eval "$(git zshadow)"`. Re-running must not re-emit a `PATH` entry the
//! environment already has, or every shell start would grow `PATH`.

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Run `git zshadow <args>` with `ZVCS_HOME` pointed at `home`, plus any extra
/// environment the case needs.
fn zshadow(home: &Path, args: &[&str], env: &[(&str, String)]) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.arg("zshadow").args(args).env("ZVCS_HOME", home);
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().expect("run git zshadow")
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-zshadow-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn installs_every_piece_of_the_shadow() {
    let home = scratch("install");
    let out = zshadow(&home, &[], &[]);
    assert!(out.status.success(), "zshadow failed: {}", String::from_utf8_lossy(&out.stderr));

    let shim = home.join("bin").join("git");
    assert!(
        std::fs::symlink_metadata(&shim).is_ok_and(|m| m.file_type().is_symlink()),
        "no git shim symlink at {}",
        shim.display()
    );
    // The dashed links point at the shim by name, so a rebuild only has to
    // repoint one link rather than all of them.
    let dashed = home.join("bin").join("git-status");
    assert_eq!(std::fs::read_link(&dashed).expect("git-status link"), Path::new("git"));
    assert!(home.join("bin").join("git-zshadow").exists(), "superset verbs need dashed links too");
    assert!(home.join("man").join("man1").join("git-zsync.1").exists(), "man pages not installed");

    let comp = home.join("completions").join("_git");
    let text = std::fs::read_to_string(&comp).expect("installed completion");
    assert!(text.starts_with("#compdef git"), "installed _git is not a zsh completion");
    assert!(text.contains("zshadow:'zvcs:"), "installed _git does not know the zshadow verb");

    // Second run is a no-op that still succeeds and rewrites nothing.
    let again = zshadow(&home, &[], &[]);
    assert!(again.status.success());
    let summary = String::from_utf8_lossy(&again.stderr);
    assert!(summary.contains("_git current"), "completion rewritten on a no-op run: {summary}");
    assert!(summary.contains("0 dashed link(s) new"), "links reinstalled on a no-op run: {summary}");

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn stdout_is_shell_code_and_stderr_carries_the_summary() {
    let home = scratch("stdout");
    let out = zshadow(&home, &[], &[]);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");

    // Exactly the three rc lines; anything else would be executed by an eval.
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 3, "stdout should be three shell lines:\n{stdout}");
    assert!(lines[0].starts_with(r#"export PATH=""#), "first line: {}", lines[0]);
    assert!(lines[1].starts_with(r#"export MANPATH=""#), "second line: {}", lines[1]);
    assert!(lines[2].starts_with("fpath=("), "third line: {}", lines[2]);
    for (line, dir) in [(lines[0], "bin"), (lines[1], "man"), (lines[2], "completions")] {
        let want = home.join(dir);
        assert!(line.contains(&want.display().to_string()), "{line} does not name {}", want.display());
    }
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("zshadow:"),
        "the install summary belongs on stderr"
    );

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_directory_already_in_the_environment_is_emitted_commented_out() {
    let home = scratch("dedupe");
    let bin = home.join("bin");
    let man = home.join("man");
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    let out = zshadow(
        &home,
        &[],
        &[("PATH", path), ("MANPATH", format!("{}:", man.display()))],
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");

    assert!(
        stdout.lines().any(|l| l.starts_with("# export PATH=") && l.ends_with("already on PATH")),
        "an eval must not re-prepend a PATH entry the shell already has:\n{stdout}"
    );
    assert!(
        stdout.lines().any(|l| l.starts_with("# export MANPATH=")),
        "same for MANPATH:\n{stdout}"
    );
    // fpath is a zsh variable, never exported, so it cannot be detected here and
    // is always emitted live.
    assert!(stdout.lines().any(|l| l.starts_with("fpath=(")), "fpath line missing:\n{stdout}");

    // --all overrides the suppression, for pasting into a fresh rc file.
    let all = zshadow(
        &home,
        &["--all"],
        &[("PATH", format!("{}:/usr/bin", bin.display()))],
    );
    let stdout = String::from_utf8(all.stdout).expect("utf-8 stdout");
    assert!(
        stdout.lines().any(|l| l.starts_with("export PATH=")),
        "--all should print every line uncommented:\n{stdout}"
    );

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn print_only_installs_nothing() {
    let home = scratch("print");
    let out = zshadow(&home, &["-n"], &[]);
    assert!(out.status.success());
    assert!(!home.join("bin").exists(), "--print must not create the bin directory");
    assert!(!home.join("completions").exists(), "--print must not write the completion");
    assert_eq!(String::from_utf8_lossy(&out.stdout).lines().count(), 3);
    assert!(out.stderr.is_empty(), "--print has nothing to summarize");

    let _ = std::fs::remove_dir_all(&home);
}
