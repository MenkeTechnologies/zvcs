//! An `:(attr:…)` pathspec element makes `git_check_attr()` run from `match_pathspec_item()`
//! before the element's path part is compared (dir.c), and the first `git_check_attr()` of a
//! run dies on a `GIT_ATTR_SOURCE` that names no tree-ish (attr.c, `compute_default_attr_source`).
//! `ls-files` listed the index regardless.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, env: &[(&str, &str)], args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .envs(env.iter().copied())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("LC_ALL", "C")
        .output()
        .expect("run git");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

fn fixture(bin: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "zvcs-lsf-attrsrc-{}-{}",
        std::process::id(),
        if bin == BIN { "zvcs" } else { "stock" }
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    run(bin, &dir, &[], &["init", "-q", "-b", "main", "."]);
    std::fs::write(dir.join("a"), "a\n").unwrap();
    std::fs::write(dir.join("sub/b"), "b\n").unwrap();
    run(bin, &dir, &[], &["add", "a", "sub/b"]);
    run(bin, &dir, &[], &["commit", "-q", "-m", "one"]);
    dir
}

#[test]
fn bad_attr_source_dies_when_an_attr_element_meets_an_entry() {
    let Some(stock) = stock_git() else { return };
    let (s, z) = (fixture(stock), fixture(BIN));
    let bad = [("GIT_ATTR_SOURCE", "does-not-exist")];
    let vectors: &[&[&str]] = &[
        &["ls-files", ":(attr:text)"],
        &["ls-files", ":(attr:text)", "a"],
        &["ls-files", ":(attr:text)zzz", "a"],
        &["ls-files", "-s", ":(attr:text)a"],
        &["ls-files", "-d", ":(attr:text)"],
        &["ls-files", "--error-unmatch", "a", ":(attr:text)", "no/such/path"],
        // every element is under a prefix that holds no entry: prune_index() leaves nothing to test
        &["ls-files", ":(attr:text)nothing/"],
        &["ls-files", "a"],
    ];
    for args in vectors {
        let want = run(stock, &s, &bad, args);
        assert_eq!(run(BIN, &z, &bad, args), want, "{args:?}");
    }
    // A source that resolves is not an error.
    let good = [("GIT_ATTR_SOURCE", "HEAD")];
    let args = ["ls-files", ":(attr:text)", "a"];
    assert_eq!(run(BIN, &z, &good, &args), run(stock, &s, &good, &args));
    let _ = (std::fs::remove_dir_all(s), std::fs::remove_dir_all(z));
}
