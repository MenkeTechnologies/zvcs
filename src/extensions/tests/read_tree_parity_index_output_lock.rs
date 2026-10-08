//! `git read-tree --index-output=<file>` against stock git when `<file>.lock` cannot be
//! created.
//!
//! The result is written through `write_locked_index()`, so a missing directory ends the
//! command with `fatal: unable to write new index file` at 128, for the plain read and
//! the merging forms alike, and it leaves no output behind.
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
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@x")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (text(&out.stdout), text(&out.stderr), out.status.code())
}

#[test]
fn an_unlockable_output_is_unable_to_write_new_index_file() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = std::env::temp_dir().join(format!("zvcs-rt-index-output-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    run(stock, &root, &["init", "-q", "-b", "main"]);
    for (name, body) in [("a", "one\n"), ("b", "two\n"), ("c", "three\n")] {
        std::fs::write(root.join(name), body).unwrap();
        run(stock, &root, &["add", name]);
        run(stock, &root, &["commit", "-qm", name]);
    }

    let in_refs = root.join(".git/refs/heads");
    let cases: &[(&Path, &[&str])] = &[
        (&root, &["read-tree", "--index-output=nodir/idx", "HEAD~1"]),
        (&root, &["read-tree", "--index-output=nodir/idx", "-m", "HEAD~1", "HEAD"]),
        (&root, &["read-tree", "--index-output=nodir/idx", "--empty"]),
        (&root, &["read-tree", "--index-output=idx-ok", "HEAD~1"]),
        (&in_refs, &["read-tree", "--index-output=.git/parity-index", "HEAD~1"]),
        (&in_refs, &["read-tree", "--index-output=../../parity-index", "HEAD~1"]),
    ];
    for (dir, args) in cases {
        let _ = std::fs::remove_file(root.join("idx-ok"));
        assert_eq!(run(BIN, dir, args), run(stock, dir, args), "{args:?} in {dir:?}");
    }
    let _ = std::fs::remove_dir_all(&root);
}
