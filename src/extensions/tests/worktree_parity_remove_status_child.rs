//! `worktree remove` asks `check_clean_worktree()` by running `git status --porcelain
//! --ignore-submodules=none` as a child with `GIT_DIR` and `GIT_WORK_TREE` set to the worktree
//! (builtin/worktree.c). The child parses the worktree's own configuration, so a value it
//! refuses makes the child die, and `remove` dies with
//! `failed to run 'git status' on '<wt>', code 128: <strerror(errno)>` leaving the worktree
//! in place.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("LC_ALL", "C")
        .output()
        .expect("run git");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

/// A repository with one commit and a linked worktree `wt` whose per-worktree configuration
/// holds `worktree_config`.
fn fixture(bin: &str, tag: &str, worktree_config: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "zvcs-wt-remove-child-{tag}-{}-{}",
        std::process::id(),
        if bin == BIN { "zvcs" } else { "stock" }
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    run(bin, &dir, &["init", "-q", "-b", "main", "."]);
    std::fs::write(dir.join("a"), "a\n").unwrap();
    run(bin, &dir, &["add", "a"]);
    run(bin, &dir, &["commit", "-q", "-m", "one"]);
    run(bin, &dir, &["config", "extensions.worktreeConfig", "true"]);
    run(bin, &dir, &["worktree", "add", "-q", "-b", "side", "wt"]);
    std::fs::write(dir.join(".git/worktrees/wt/config.worktree"), worktree_config).unwrap();
    dir
}

#[test]
fn a_status_child_that_dies_makes_remove_die() {
    let Some(stock) = stock_git() else { return };
    for config in ["[color]\n\tdiff = \" \"\n", "[status]\n\tshowUntrackedFiles = nonsense\n", ""] {
        let (s, z) = (fixture(stock, "dies", config), fixture(BIN, "dies", config));
        // The per-worktree config path appears in the diagnostic; the two fixtures differ only there.
        let norm = |(o, e, c): (String, String, i32), dir: &Path| (o, e.replace(dir.to_str().unwrap(), "<R>"), c);
        let want = norm(run(stock, &s, &["worktree", "remove", "wt"]), &s);
        let got = norm(run(BIN, &z, &["worktree", "remove", "wt"]), &z);
        assert_eq!(got, want, "config {config:?}");
        assert_eq!(s.join("wt").exists(), z.join("wt").exists(), "config {config:?}: worktree left in place");
        let _ = (std::fs::remove_dir_all(s), std::fs::remove_dir_all(z));
    }
}

#[test]
fn an_untracked_file_still_refuses_and_force_still_removes() {
    let Some(stock) = stock_git() else { return };
    let (s, z) = (fixture(stock, "untracked", ""), fixture(BIN, "untracked", ""));
    for d in [&s, &z] {
        std::fs::write(d.join("wt/untracked"), "u\n").unwrap();
    }
    assert_eq!(run(BIN, &z, &["worktree", "remove", "wt"]), run(stock, &s, &["worktree", "remove", "wt"]));
    assert_eq!(
        run(BIN, &z, &["worktree", "remove", "--force", "wt"]),
        run(stock, &s, &["worktree", "remove", "--force", "wt"])
    );
    assert!(!z.join("wt").exists() && !s.join("wt").exists());
    let _ = (std::fs::remove_dir_all(s), std::fs::remove_dir_all(z));
}
