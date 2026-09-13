//! `git config --list` walks the command line the way `git_config_from_parameters()`
//! (config.c:731-790) hands it over: the `GIT_CONFIG_COUNT` entries first, then
//! every `-c` in argv order, valued and valueless interleaved, each with the bytes
//! as typed and the `kvi_from_param()` origin (config.c:642-647), which
//! `--show-origin` names `command line` for both (config.c:3604-3605).
//!
//! The port hands a valued `-c` to gitoxide twice — `Source::Cli` and the
//! environment triple — and a valueless one once, so a walk over the snapshot
//! listed every valued `-c` ahead of every valueless one and labelled it
//! `environment:`. A scoped read never sees the command line at all
//! (`config_with_options()`, config.c:1634-1645), so `--file` must keep an entry
//! that happens to equal a `-c`.
//!
//! Expectations were taken from stock git 2.55.0 under the same environment.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn scratch(tag: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let root = std::env::temp_dir()
        .join(format!("zvcs-cliorder-{tag}-{}-{unique}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home")).expect("mkdir home");
    std::fs::create_dir_all(root.join("w")).expect("mkdir work");
    root.canonicalize().expect("canonicalize fixture")
}

/// Outside any repository, with no system or global configuration, so only the
/// command line (and whatever `env` adds) is listed.
fn run(root: &Path, env: &[(&str, &str)], args: &[&str]) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .current_dir(root.join("w"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root.join("home"))
        .env("ZVCS_HOME", root.join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CEILING_DIRECTORIES", root);
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().expect("run zvcs git")
}

#[test]
fn list_walks_environment_then_argv_in_order() {
    let root = scratch("list");
    let env = [("GIT_CONFIG_COUNT", "1"), ("GIT_CONFIG_KEY_0", "e.k"), ("GIT_CONFIG_VALUE_0", "v")];
    let argv = ["-c", "a.b", "-c", "a.c=1 ", "-c", "a.d", "config", "--list"];

    let plain = run(&root, &env, &argv);
    assert_eq!(String::from_utf8_lossy(&plain.stdout), "e.k=v\na.b\na.c=1 \na.d\n");

    let mut shown = argv.to_vec();
    shown.extend(["--show-scope", "--show-origin"]);
    let shown = run(&root, &env, &shown);
    assert_eq!(
        String::from_utf8_lossy(&shown.stdout),
        "command\tcommand line:\te.k=v\n\
         command\tcommand line:\ta.b\n\
         command\tcommand line:\ta.c=1 \n\
         command\tcommand line:\ta.d\n"
    );

    let mut names = argv.to_vec();
    names.push("--name-only");
    let names = run(&root, &env, &names);
    assert_eq!(String::from_utf8_lossy(&names.stdout), "e.k\na.b\na.c\na.d\n");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_named_file_keeps_an_entry_equal_to_a_command_line_override() {
    let root = scratch("file");
    std::fs::write(root.join("w/x.cfg"), "[a]\n\tb = 1\n").expect("write x.cfg");
    let out = run(&root, &[], &["-c", "a.b=1", "config", "--file", "x.cfg", "--list"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a.b=1\n");
    assert!(out.status.success());
    let _ = std::fs::remove_dir_all(&root);
}
