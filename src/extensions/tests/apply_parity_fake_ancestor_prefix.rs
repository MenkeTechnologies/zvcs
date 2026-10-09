//! `git apply --build-fake-ancestor=<file>` from a subdirectory.
//!
//! `OPT_FILENAME` runs the value through `fix_filename(prefix, …)`, so a relative `<file>` names
//! a file under the directory `apply` was typed in (`apply` has stood at the top of the work
//! tree by then). zvcs wrote it relative to the top level instead.

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
        .env("HOME", dir)
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    let root = dir.to_string_lossy();
    let scrub = |b: &[u8]| String::from_utf8_lossy(b).replace(root.as_ref(), "<ROOT>");
    (scrub(&out.stdout), scrub(&out.stderr), out.status.code())
}

fn fixture(tag: &str, stock: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-fakeanc-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let dir = dir.canonicalize().unwrap();
    std::fs::write(dir.join("src/a"), "a\n").unwrap();
    run(stock, &dir, &["init", "-q", "-b", "main"]);
    run(stock, &dir, &["add", "."]);
    assert_eq!(run(stock, &dir, &["commit", "-qm", "one"]).2, Some(0));
    std::fs::write(dir.join("src/a"), "b\n").unwrap();
    let patch = run(stock, &dir, &["diff"]).0;
    std::fs::write(dir.join("p.patch"), patch).unwrap();
    run(stock, &dir, &["checkout", "-q", "--", "."]);
    dir
}

#[test]
fn a_relative_fake_ancestor_is_relative_to_where_apply_was_typed() {
    let Some(stock) = stock_git() else { return };
    let cases: [&[&str]; 4] = [
        &["apply", "--build-fake-ancestor=fa.idx", "../p.patch"],
        &["apply", "--build-fake-ancestor", "fa.idx", "../p.patch"],
        &["apply", "--build-fake-ancestor=.git/x.idx", "../p.patch"],
        &["apply", "--build-fake-ancestor=../top.idx", "../p.patch"],
    ];
    for args in cases {
        let mut seen = Vec::new();
        for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
            let dir = fixture(who, stock);
            let result = run(bin, &dir.join("src"), args);
            let files = (
                dir.join("src/fa.idx").exists(),
                dir.join("fa.idx").exists(),
                dir.join("top.idx").exists(),
            );
            let _ = std::fs::remove_dir_all(&dir);
            seen.push((result, files));
        }
        assert_eq!(seen[1], seen[0], "args {args:?}");
    }
}
