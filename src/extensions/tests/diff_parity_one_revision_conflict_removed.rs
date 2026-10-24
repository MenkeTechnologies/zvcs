//! `git diff <rev>` over a conflicted path.
//!
//! `unpack_trees()` hands `oneway_diff()` the path's lowest stage as `idx`, and
//! `show_modified()` asks `get_stat_data()` (diff-lib.c:315-342) for the worktree
//! side: `check_removed()` finding no file returns -1, which `report_missing` turns
//! into the tree entry's deletion (diff-lib.c:400-406); a file that is there takes
//! `ce_mode_from_stat(idx, st.st_mode)` — the worktree's executable bit under
//! `core.fileMode`. zvcs read the missing file and died with `No such file or
//! directory`, reported `M` in `--raw`, and kept the stage's mode.
//!
//! `git diff` with no revision goes through the same `check_removed()` and
//! `ce_mode_from_stat()` for the `U` pair's mode and for the stage pair after it
//! (diff-lib.c:157-167, :258-273), which zvcs answered from the bare `lstat()`.

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

/// `aa.txt` (executable in the worktree after the merge) and `conflict.txt`
/// conflict; `remove` deletes `conflict.txt` from the worktree.
fn fixture(tag: &str, stock: &str, remove: bool) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-onerevconflict-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    run(stock, &dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("conflict.txt"), "base\nline2\n").unwrap();
    std::fs::write(dir.join("aa.txt"), "aa\n").unwrap();
    run(stock, &dir, &["add", "."]);
    run(stock, &dir, &["commit", "-qm", "base"]);
    run(stock, &dir, &["checkout", "-qb", "side"]);
    std::fs::write(dir.join("conflict.txt"), "side\nline2\n").unwrap();
    std::fs::write(dir.join("aa.txt"), "aaside\n").unwrap();
    run(stock, &dir, &["update-index", "--chmod=+x", "aa.txt"]);
    run(stock, &dir, &["commit", "-qam", "side"]);
    run(stock, &dir, &["checkout", "-q", "main"]);
    std::fs::write(dir.join("conflict.txt"), "main\nline2\n").unwrap();
    std::fs::write(dir.join("aa.txt"), "aamain\n").unwrap();
    run(stock, &dir, &["commit", "-qam", "main"]);
    assert_eq!(run(stock, &dir, &["merge", "-q", "side"]).2, Some(1));
    if remove {
        std::fs::remove_file(dir.join("conflict.txt")).unwrap();
    }
    dir
}

const DIFFS: [&[&str]; 20] = [
    &["diff", "HEAD"],
    &["diff", "HEAD", "--raw"],
    &["diff", "HEAD", "--name-status"],
    &["diff", "HEAD", "--stat"],
    &["diff", "HEAD", "--numstat"],
    &["diff", "HEAD", "--summary"],
    &["diff", "HEAD", "--cc"],
    &["diff", "HEAD", "-c", "--raw"],
    &["diff", "HEAD", "--check"],
    &["diff", "HEAD", "--exit-code"],
    &["diff", "HEAD", "--", "conflict.txt"],
    &["diff", "-R", "HEAD", "--raw"],
    &["diff", "HEAD^", "--raw"],
    &["diff", "side", "--raw"],
    &["-c", "core.fileMode=false", "diff", "HEAD", "--raw"],
    &["diff", "--raw"],
    &["diff", "--base", "--raw"],
    &["-c", "core.fileMode=false", "diff", "--raw"],
    &["-c", "core.fileMode=false", "diff", "--theirs"],
    &["diff-index", "HEAD"],
];

#[test]
fn a_conflicted_path_takes_its_worktree_side_from_get_stat_data() {
    let Some(stock) = stock_git() else { return };
    for remove in [false, true] {
        let mut seen = Vec::new();
        for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
            let dir = fixture(&format!("{remove}-{who}"), stock, remove);
            seen.push(DIFFS.iter().map(|args| run(bin, &dir, args)).collect::<Vec<_>>());
            let _ = std::fs::remove_dir_all(&dir);
        }
        for (i, args) in DIFFS.iter().enumerate() {
            assert_eq!(seen[1][i], seen[0][i], "remove={remove}: {args:?}");
        }
    }
}
