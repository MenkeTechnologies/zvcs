//! `zforeach` fans a command across all/subset of indexed repos.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(Command::new("git").args(args).current_dir(dir).status().unwrap().success(), "git {args:?} failed");
}

fn zvcs(home: &Path, cwd: &Path, args: &[&str]) -> String {
    let out = Command::new(BIN).args(args).current_dir(cwd).env("ZVCS_HOME", home).output().unwrap();
    // zforeach prints per-repo groups on stdout.
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn zforeach_runs_across_all_and_subset() {
    let root = std::env::temp_dir().join(format!("zvcs-foreach-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");

    for name in ["alpha", "beta"] {
        let r = root.join(name);
        std::fs::create_dir_all(&r).unwrap();
        git(&r, &["init", "-q", "-b", "main"]);
        git(&r, &["commit", "--allow-empty", "-q", "-m", "c0"]);
    }
    // Index both.
    assert!(Command::new(BIN).args(["zreindex", "--sync", root.to_str().unwrap()]).current_dir(&root).env("ZVCS_HOME", &home).status().unwrap().success());

    // Across all: both repos appear.
    let all = zvcs(&home, &root, &["zforeach", "--", "git", "rev-parse", "HEAD"]);
    assert!(all.contains("alpha") && all.contains("beta"), "zforeach all:\n{all}");

    // Subset by --repo: only alpha.
    let sub = zvcs(&home, &root, &["zforeach", "--repo", "alpha", "--", "git", "rev-parse", "HEAD"]);
    assert!(sub.contains("alpha"), "subset missing alpha:\n{sub}");
    assert!(!sub.contains("beta"), "subset should exclude beta:\n{sub}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn double_dash_shields_command_args_from_selector_parsing() {
    // The command after `--` may itself contain selector-looking flags. They must
    // reach the command, NOT be consumed as selectors (which would both mangle the
    // command and narrow the repo set — running the wrong command on the wrong set).
    let root = std::env::temp_dir().join(format!("zvcs-fedd-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");

    for name in ["alpha", "beta"] {
        let r = root.join(name);
        std::fs::create_dir_all(&r).unwrap();
        git(&r, &["init", "-q", "-b", "main"]);
        git(&r, &["commit", "--allow-empty", "-q", "-m", "c0"]);
    }
    assert!(Command::new(BIN).args(["zreindex", "--sync", root.to_str().unwrap()]).current_dir(&root).env("ZVCS_HOME", &home).status().unwrap().success());

    // `--session prod` here is part of the COMMAND (echo's args), not a selector.
    // No repo is claimed by "prod", so the buggy path narrows to zero → "no repos
    // matched" and drops the command's args.
    let out = zvcs(&home, &root, &["zforeach", "--", "echo", "hi", "--session", "prod"]);
    assert!(!out.contains("no repos matched"), "command's --session was wrongly parsed as a selector:\n{out}");
    assert!(out.contains("alpha") && out.contains("beta"), "must run across ALL repos:\n{out}");
    assert!(out.contains("--session prod"), "the command must receive its own --session arg:\n{out}");

    let _ = std::fs::remove_dir_all(&root);
}
