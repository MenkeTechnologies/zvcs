//! Every command-line setting is read once, in `git_config_from_parameters()` order.
//!
//! git publishes each `-c` (and `--config-env`) by appending it to
//! `GIT_CONFIG_PARAMETERS` (`git_config_push_split_parameter()`, config.c:449-467),
//! and every configuration read takes the command line from the environment:
//! the `GIT_CONFIG_COUNT` / `_KEY_<n>` / `_VALUE_<n>` entries first, then each
//! `GIT_CONFIG_PARAMETERS` entry (`git_config_from_parameters()`, config.c:731-790).
//! Children inherit both variables untouched, so a hook or a `submodule foreach`
//! body sees each `-c` once. zvcs used to write every valued `-c` into the
//! `GIT_CONFIG_COUNT` triple as well, so a child read it twice, and a stock
//! `git config --get-all` in a hook listed `1 2 1 2` for `-c x.y=1 -c x.y=2`.
//!
//! Each case runs the same command through stock git and through this binary on
//! identical fixtures and compares stdout, stderr and the exit code. Children
//! spawned by the hook and the `foreach` body are stock git, named by absolute
//! path, so the comparison is about what the parent hands down.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn git(bin: &str, cwd: &Path, home: &Path, env: &[(&str, &str)], args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("GIT_CEILING_DIRECTORIES", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .env("TERM", "dumb")
        .envs(env.iter().copied())
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// Two identical repositories, `stock/` and `zvcs/`, each with one commit and a
/// `pre-commit` hook that reads the command line back through stock git.
fn fixture(stock: &str, tag: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!("zvcs-cfg-read-once-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    for side in ["stock", "zvcs"] {
        let dir = root.join(side);
        std::fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str]| {
            let (_, err, code) = git(stock, &dir, &root, &[], args);
            assert_eq!(code, 0, "{args:?}: {err}");
        };
        run(&["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("a"), "a\n").unwrap();
        run(&["add", "a"]);
        run(&["-c", "maintenance.auto=false", "commit", "-q", "-m", "a"]);
        let hook = dir.join(".git/hooks/pre-commit");
        std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
        std::fs::write(
            &hook,
            format!(
                "#!/bin/sh\n{stock} config --get-all --show-origin --show-scope x.y\n\
                 echo \"count=${{GIT_CONFIG_COUNT-unset}}\"\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    Fixture { root }
}

fn compare(stock: &str, f: &Fixture, env: &[(&str, &str)], args: &[&str]) {
    let expected = git(stock, &f.root.join("stock"), &f.root, env, args);
    let actual = git(ZVCS, &f.root.join("zvcs"), &f.root, env, args);
    assert_eq!(actual, expected, "git {args:?} with {env:?}");
}

/// A hook is a child: it inherits `GIT_CONFIG_PARAMETERS` holding each `-c` once and
/// the `GIT_CONFIG_COUNT` the parent was given, unchanged.
#[test]
fn a_hook_reads_each_command_line_value_once() {
    let Some(stock) = stock_git() else { return };
    let f = fixture(stock, "hook");
    let commit = ["-c", "x.y=1", "-c", "x.y", "-c", "x.y=2", "-c", "maintenance.auto=false"];
    let args: Vec<&str> = commit.iter().copied().chain(["commit", "-q", "--allow-empty", "-m", "b"]).collect();
    compare(stock, &f, &[], &args);
    let env = [("GIT_CONFIG_COUNT", "1"), ("GIT_CONFIG_KEY_0", "x.y"), ("GIT_CONFIG_VALUE_0", "env")];
    compare(stock, &f, &env, &args);
}

/// The process's own read: inherited `GIT_CONFIG_COUNT` entries, then the inherited
/// `GIT_CONFIG_PARAMETERS` entries, then this command line, valued and valueless
/// interleaved, with the bytes as typed. A key holding an `=` and a value holding a
/// `!` survive the round trip through `sq_quote_buf()` (quote.c:28-48) into the
/// `!`-alias child.
#[test]
fn the_merged_read_lists_the_command_line_once_in_order() {
    let Some(stock) = stock_git() else { return };
    let f = fixture(stock, "list");
    let env = [
        ("GIT_CONFIG_COUNT", "1"),
        ("GIT_CONFIG_KEY_0", "x.y"),
        ("GIT_CONFIG_VALUE_0", "env"),
        ("GIT_CONFIG_PARAMETERS", "'x.y'='inherited' 'x.y' 'x.z=old style'"),
    ];
    let list = ["config", "--list", "--show-origin", "--show-scope"];
    let line = ["-c", "x.y=a ", "-c", "x.y", "--config-env=s.sub=eq.k=HOME"];
    let args: Vec<&str> = line.iter().copied().chain(list).collect();
    compare(stock, &f, &env, &args);
    compare(stock, &f, &[], &["-c", "x.y=1", "-c", "x.y=2", "config", "--get-all", "--show-origin", "x.y"]);
    compare(
        stock,
        &f,
        &[],
        &["-c", "x.y=b!ang", "-c", "alias.child=!git config --get-all x.y", "child"],
    );
}

/// The multi-valued remote keys count one entry per `-c`.
#[test]
fn remote_multivars_from_the_command_line_count_once() {
    let Some(stock) = stock_git() else { return };
    let f = fixture(stock, "remote");
    let remote = [
        "-c", "remote.origin.url=/x",
        "-c", "remote.origin.pushurl=/p",
        "-c", "remote.origin.pushurl=/q",
        "-c", "remote.origin.push=refs/heads/main:refs/heads/one",
        "-c", "remote.origin.fetch=+refs/heads/*:refs/remotes/origin/*",
    ];
    let args: Vec<&str> = remote.iter().copied().chain(["remote", "-v"]).collect();
    compare(stock, &f, &[], &args);
    let args: Vec<&str> = remote.iter().copied().chain(["remote", "show", "-n", "origin"]).collect();
    compare(stock, &f, &[], &args);
}
