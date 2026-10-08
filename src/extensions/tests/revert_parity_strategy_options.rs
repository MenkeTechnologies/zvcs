//! `git revert -X<option>` against stock git. `do_recursive_merge()` hands every
//! strategy option to `parse_merge_opt()` for a revert exactly as it does for a
//! pick, so the whitespace rules and the `ours`/`theirs` favour decide the
//! merge, and an unknown option is dropped without a word.
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
        .env("GIT_EDITOR", "true")
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

fn commit(stock: &str, root: &Path, body: &str, msg: &str) {
    std::fs::write(root.join("f.c"), body).unwrap();
    run(stock, root, &["add", "f.c"]);
    run(stock, root, &["commit", "-qm", msg]);
}

/// Four commits on `f.c`: seed, a whitespace-only rewrite (`ws`), a real edit
/// on top of different indentation, and a second real edit to the same line.
fn fixture(stock: &str, root: &Path) {
    std::fs::create_dir_all(root).unwrap();
    run(stock, root, &["init", "-q", "-b", "main"]);
    commit(stock, root, "int f(void)\n{\n\tint a = 0;\n\treturn a;\n}\n", "seed");
    commit(stock, root, "int f(void)\n{\n    int a = 0;   \n    return a;\n}\n", "ws");
    commit(stock, root, "int f(void)\n{\n  int a = 1;\n  return a;\n}\n", "edit one");
    commit(stock, root, "int f(void)\n{\n  int a = 2;\n  return a;\n}\n", "edit two");
}

#[test]
fn strategy_options_reach_the_revert_merge_like_stock() {
    let Some(stock) = stock_git::stock_git() else { return };
    let base = std::env::temp_dir().join(format!("zvcs-revert-xopts-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let cases: &[&[&str]] = &[
        // Only whitespace differs between the reverted commit and its parent.
        &["revert", "--no-commit", "--no-edit", "-Xignore-space-change", "HEAD~2"],
        &["revert", "--no-commit", "--no-edit", "-Xignore-all-space", "HEAD~2"],
        &["revert", "--no-commit", "--no-edit", "--strategy-option=ignore-space-at-eol", "HEAD~2"],
        &["revert", "--no-commit", "--no-edit", "HEAD~2"],
        // A genuine conflict: the favoured side wins, an unknown option changes nothing.
        &["revert", "--no-commit", "--no-edit", "-Xours", "HEAD~1"],
        &["revert", "--no-commit", "--no-edit", "-Xtheirs", "HEAD~1"],
        &["revert", "--no-commit", "--no-edit", "-Xbogus", "HEAD~1"],
        &["revert", "--no-edit", "-Xtheirs", "HEAD~1"],
    ];
    for args in cases {
        let (s, z) = (base.join("stock"), base.join("zvcs"));
        for root in [&s, &z] {
            let _ = std::fs::remove_dir_all(root);
            fixture(stock, root);
        }
        let want = run(stock, &s, args);
        let got = run(BIN, &z, args);
        assert_eq!(got, want, "{args:?}");
        for probe in [&["status", "--porcelain"][..], &["log", "--format=%H %s"][..]] {
            assert_eq!(run(BIN, &z, probe), run(stock, &s, probe), "{probe:?} after {args:?}");
        }
        assert_eq!(
            std::fs::read(z.join("f.c")).unwrap(),
            std::fs::read(s.join("f.c")).unwrap(),
            "f.c after {args:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&base);
}
