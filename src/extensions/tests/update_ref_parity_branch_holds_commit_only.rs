//! `git update-ref` against stock git: a branch (`HEAD` or `refs/heads/*`) only
//! ever holds a commit. `ref_transaction_update()` refuses any other object type
//! — an annotated tag aimed at a commit included — before anything is written,
//! on the command line, through `--stdin`, with and without `--no-deref`. Refs
//! outside `refs/heads/` take any object.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

type Outcome = (String, String, Option<i32>);

fn run(bin: &str, dir: &Path, args: &[&str], stdin: &str) -> Outcome {
    let mut child = Command::new(bin)
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
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (text(&out.stdout), text(&out.stderr), out.status.code())
}

fn fixture(stock: &str, root: &Path) {
    std::fs::create_dir_all(root).unwrap();
    let git = |args: &[&str]| run(stock, root, args, "");
    git(&["init", "-q", "-b", "main"]);
    git(&["commit", "-q", "--allow-empty", "-m", "one"]);
    git(&["tag", "-a", "-m", "annotated", "ann"]);
}

#[test]
fn a_branch_refuses_a_non_commit_object_like_stock() {
    let Some(stock) = stock_git::stock_git() else { return };
    let base = std::env::temp_dir().join(format!("zvcs-updref-commit-only-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let cases: &[(&[&str], &str)] = &[
        (&["update-ref", "refs/heads/new", "ann"], ""),
        (&["update-ref", "--no-deref", "refs/heads/new", "ann"], ""),
        (&["update-ref", "HEAD", "ann"], ""),
        (&["update-ref", "refs/heads/main", "main^{tree}"], ""),
        (&["update-ref", "refs/heads/new", "ann^{tree}", "-m", "msg"], ""),
        (&["update-ref", "--stdin"], "create refs/heads/new ann\n"),
        (&["update-ref", "--stdin"], "update refs/heads/main ann\n"),
        (&["update-ref", "refs/tags/other", "ann"], ""),
        (&["update-ref", "refs/notes/x", "main^{tree}"], ""),
        (&["update-ref", "refs/heads/new", "1234567890123456789012345678901234567890"], ""),
    ];
    for (args, input) in cases {
        let (s, z) = (base.join("stock"), base.join("zvcs"));
        for root in [&s, &z] {
            let _ = std::fs::remove_dir_all(root);
            fixture(stock, root);
        }
        let want = run(stock, &s, args, input);
        let got = run(BIN, &z, args, input);
        assert_eq!(got, want, "{args:?} {input:?}");
        let refs = |bin: &str, root: &Path| run(bin, root, &["for-each-ref"], "").0;
        assert_eq!(refs(BIN, &z), refs(stock, &s), "refs after {args:?}");
    }
    let _ = std::fs::remove_dir_all(&base);
}
