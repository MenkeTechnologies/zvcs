//! `rebase --skip` / `--abort` restore the worktree through `reset_head()` with
//! `RESET_HEAD_HARD`, i.e. `unpack_trees()` with `oneway_merge` and `o.reset`. Its
//! `same(old, a)` arm (unpack-trees.c:3036-3048) keeps an entry whose stat still
//! matches, so a file the restore does not change is not rewritten. zvcs checked
//! every entry out again, which turned LF files CRLF once `core.autocrlf=true` was
//! set after they were written.
//!
//! A defaulted `<upstream>` goes through `branch_get(NULL)` (builtin/rebase.c:1654),
//! whose remote.c `read_config()` dies on `remote.origin.prune==`.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> (String, String, Option<i32>) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("GIT_EDITOR", "true")
        .env("HOME", dir)
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
    )
}

/// `main` and `theirs` both add `conflict.txt`, so rebasing `main` onto `theirs`
/// stops; `base.txt` is the same blob in every commit and was written with LF.
fn stopped(tag: &str, stock: &str, bin: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-rbskip-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    run(stock, &dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("base.txt"), "base\n").unwrap();
    run(stock, &dir, &["add", "."]);
    run(stock, &dir, &["commit", "-qm", "base"]);
    run(stock, &dir, &["checkout", "-q", "-b", "theirs"]);
    std::fs::write(dir.join("conflict.txt"), "theirs\n").unwrap();
    run(stock, &dir, &["add", "."]);
    run(stock, &dir, &["commit", "-qm", "theirs"]);
    run(stock, &dir, &["checkout", "-q", "main"]);
    std::fs::write(dir.join("conflict.txt"), "ours\n").unwrap();
    run(stock, &dir, &["add", "."]);
    run(stock, &dir, &["commit", "-qm", "ours"]);
    run(stock, &dir, &["config", "core.autocrlf", "true"]);
    assert_eq!(run(bin, &dir, &["rebase", "theirs"]).2, Some(1));
    dir
}

#[test]
fn skip_and_abort_leave_unchanged_files_alone() {
    let Some(stock) = stock_git() else { return };
    for finish in ["--skip", "--abort"] {
        let mut seen = Vec::new();
        for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
            let dir = stopped(&format!("{}-{who}", &finish[2..]), stock, bin);
            let result = run(bin, &dir, &["rebase", finish]);
            let base = std::fs::read(dir.join("base.txt")).unwrap();
            let _ = std::fs::remove_dir_all(&dir);
            seen.push((result.2, base));
        }
        assert_eq!(seen[1], seen[0], "{finish}");
        assert_eq!(seen[0].1, b"base\n", "{finish}");
    }
}

#[test]
fn defaulted_upstream_reads_remote_config() {
    let Some(stock) = stock_git() else { return };
    let mut seen = Vec::new();
    for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
        let dir = std::env::temp_dir().join(format!("zvcs-rbprune-{who}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        run(stock, &dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("a"), "a\n").unwrap();
        run(stock, &dir, &["add", "."]);
        run(stock, &dir, &["commit", "-qm", "a"]);
        seen.push(run(bin, &dir, &["-c", "remote.origin.prune==", "rebase"]));
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert_eq!(seen[1], seen[0]);
    assert_eq!(seen[0].2, Some(128));
}
