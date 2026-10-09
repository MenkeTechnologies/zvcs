//! `git rebase --skip` and `--abort` open with `rerere_clear()`.
//!
//! `cmd_rebase()` calls `rerere_clear(the_repository, &merge_rr)` before either action does
//! anything else, and `setup_rerere()` dies on a `rerere.*` value that is not a boolean. The
//! rebase state is therefore untouched and the exit is 128. zvcs skipped or aborted regardless.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, envs: &[(&str, &str)], args: &[&str]) -> (String, String, Option<i32>) {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_EDITOR", "true")
        .env("HOME", dir)
        .env("LC_ALL", "C");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
    )
}

/// `main` and `theirs` both add `conflict.txt`; the rebase of `main` is left stopped.
fn stopped(tag: &str, stock: &str, bin: &str, backend: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-rebclear-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    run(stock, &dir, &[], &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("base.txt"), "base\n").unwrap();
    run(stock, &dir, &[], &["add", "."]);
    run(stock, &dir, &[], &["commit", "-qm", "base"]);
    run(stock, &dir, &[], &["checkout", "-q", "-b", "theirs"]);
    std::fs::write(dir.join("conflict.txt"), "theirs\n").unwrap();
    run(stock, &dir, &[], &["add", "."]);
    run(stock, &dir, &[], &["commit", "-qm", "theirs"]);
    run(stock, &dir, &[], &["checkout", "-q", "main"]);
    std::fs::write(dir.join("conflict.txt"), "ours\n").unwrap();
    run(stock, &dir, &[], &["add", "."]);
    run(stock, &dir, &[], &["commit", "-qm", "ours"]);
    assert_eq!(run(bin, &dir, &[], &["rebase", backend, "theirs"]).2, Some(1));
    dir
}

#[test]
fn a_rerere_value_git_refuses_stops_skip_and_abort_before_they_act() {
    let Some(stock) = stock_git() else { return };
    let bad: &[(&str, &str)] =
        &[("GIT_CONFIG_COUNT", "1"), ("GIT_CONFIG_KEY_0", "rerere.autoUpdate"), ("GIT_CONFIG_VALUE_0", "warn")];
    for backend in ["--merge", "--apply"] {
        for action in ["--skip", "--abort"] {
            for env in [bad, &[]] {
                let mut seen = Vec::new();
                for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
                    let dir = stopped(&format!("{who}"), stock, bin, backend);
                    let result = run(bin, &dir, env, &["rebase", action]);
                    let state = (
                        dir.join(".git/rebase-merge").exists(),
                        dir.join(".git/rebase-apply").exists(),
                        run(stock, &dir, &[], &["rev-parse", "HEAD^{tree}"]).0,
                    );
                    let _ = std::fs::remove_dir_all(&dir);
                    seen.push((result, state));
                }
                assert_eq!(seen[1], seen[0], "{backend} {action} env {env:?}");
            }
        }
    }
}
