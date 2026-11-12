//! `git mv` against stock git: what `-k` does with a skip-worktree source named
//! without `--sparse`, and a destination the index cannot hold.
//!
//! A source that is not on disk because its entry is skip-worktree is listed
//! for the sparse advice and left in the move list with `bad` unset
//! (builtin/mv.c:337-340), so under `-k`, after the advice (:566-572), it is
//! still moved: needs_worktree_rename() tries the rename(), which fails and is
//! skipped after the `-v` line, unless the destination is outside the cone, in
//! which case the entry moves in the index alone (:52-56, :585-611). zvcs
//! dropped it. A destination such as `.git/x` is renamed on disk, then
//! add_index_entry_with_check() refuses it with `error: invalid path`
//! (read-cache.c:1299-1302) after the source entry is gone, and the exit is 0;
//! zvcs recorded `.git/x` in the index.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

type Outcome = (String, String, Option<i32>);

fn run(bin: &str, dir: &Path, args: &[&str]) -> Outcome {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("GIT_EDITOR", "true")
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@x")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (text(&out.stdout), text(&out.stderr), out.status.code())
}

/// `root` and `in/a` inside a cone that is just `in`; `out/c` and `out/deep/d`
/// outside it.
fn fixture(stock: &str, root: &Path) {
    for dir in ["in", "out/deep"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    for file in ["root", "in/a", "out/c", "out/deep/d"] {
        std::fs::write(root.join(file), format!("{file}\n")).unwrap();
    }
    run(stock, root, &["init", "-q", "-b", "main"]);
    run(stock, root, &["add", "."]);
    run(stock, root, &["-c", "maintenance.auto=false", "commit", "-q", "-m", "one"]);
    run(stock, root, &["sparse-checkout", "set", "in"]);
}

#[test]
fn k_moves_kept_sparse_entries_and_an_invalid_destination_leaves_the_index() {
    let Some(stock) = stock_git::stock_git() else { return };
    let base = std::env::temp_dir().join(format!("zvcs-mv-keep-going-sparse-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (s, z) = (base.join("stock"), base.join("zvcs"));
    let cases: &[&[&str]] = &[
        &["mv", "-k", "out/c", "out/c2"],
        &["mv", "-k", "-v", "out/c", "in/c"],
        &["mv", "-k", "-v", "out/deep/d", "root2"],
        &["mv", "root", ".git/x"],
        &["mv", "-k", "-v", "root", "in/a", ".git"],
    ];
    for args in cases {
        for root in [&s, &z] {
            let _ = std::fs::remove_dir_all(root);
            fixture(stock, root);
        }
        assert_eq!(run(BIN, &z, args), run(stock, &s, args), "{args:?}");
        for state in [&["ls-files", "-s"][..], &["ls-files", "-t"], &["status", "--porcelain"]] {
            assert_eq!(run(stock, &z, state), run(stock, &s, state), "{state:?} after {args:?}");
        }
        assert_eq!(z.join(".git/x").exists(), s.join(".git/x").exists(), "{args:?}");
    }
    let _ = std::fs::remove_dir_all(&base);
}
