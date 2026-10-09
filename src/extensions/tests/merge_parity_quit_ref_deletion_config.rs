//! `git merge --quit` finishes with `refs_delete_ref(…, "AUTO_MERGE", …)`.
//!
//! The files backend takes the `packed-refs` lock when it prepares that transaction
//! (`core.packedRefsTimeout`) and reads its write options when it finishes it
//! (`core.logAllRefUpdates`), whether or not `AUTO_MERGE` exists, so a value either refuses
//! is the `fatal:` and 128 — after `MERGE_HEAD` and the other state files were unlinked.
//! zvcs dropped the error of its own deletion and exited 0.

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

fn conflicted(tag: &str, stock: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-mergequit-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    run(stock, &dir, &[], &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("f.txt"), "base\n").unwrap();
    run(stock, &dir, &[], &["add", "."]);
    run(stock, &dir, &[], &["commit", "-qm", "base"]);
    run(stock, &dir, &[], &["checkout", "-q", "-b", "theirs"]);
    std::fs::write(dir.join("f.txt"), "theirs\n").unwrap();
    run(stock, &dir, &[], &["commit", "-qam", "theirs"]);
    run(stock, &dir, &[], &["checkout", "-q", "main"]);
    std::fs::write(dir.join("f.txt"), "ours\n").unwrap();
    run(stock, &dir, &[], &["commit", "-qam", "ours"]);
    assert_eq!(run(stock, &dir, &[], &["merge", "theirs"]).2, Some(1));
    dir
}

fn state_files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir.join(".git"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("MERGE_") || n == "AUTO_MERGE")
        .collect();
    names.sort();
    names
}

#[test]
fn a_ref_store_config_value_git_refuses_ends_merge_quit_after_the_unlinks() {
    let Some(stock) = stock_git() else { return };
    let cases: [&[(&str, &str)]; 4] = [
        &[("GIT_CONFIG_COUNT", "1"), ("GIT_CONFIG_KEY_0", "core.logAllRefUpdates"), ("GIT_CONFIG_VALUE_0", "none")],
        &[("GIT_CONFIG_COUNT", "1"), ("GIT_CONFIG_KEY_0", "core.packedRefsTimeout"), ("GIT_CONFIG_VALUE_0", "always")],
        &[("GIT_CONFIG_COUNT", "1"), ("GIT_CONFIG_KEY_0", "core.logAllRefUpdates"), ("GIT_CONFIG_VALUE_0", "true")],
        &[],
    ];
    for env in cases {
        let mut seen = Vec::new();
        for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
            let dir = conflicted(who, stock);
            let result = run(bin, &dir, env, &["merge", "--quit"]);
            let left = state_files(&dir);
            let _ = std::fs::remove_dir_all(&dir);
            seen.push((result, left));
        }
        assert_eq!(seen[1], seen[0], "env {env:?}");
    }
}
