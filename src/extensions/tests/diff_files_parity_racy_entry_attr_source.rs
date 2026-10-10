//! `ie_match_stat()` falls through to `ce_modified_check_fs()` for a racily clean entry
//! (mtime not older than the index's own), which hashes the file through `index_fd()`; the
//! first attribute lookup of that read dies on a `GIT_ATTR_SOURCE` naming no tree-ish
//! (attr.c `compute_default_attr_source`). `diff-files` and `diff-index` (without `--cached`)
//! stat every entry in the pathspec, so they die with a raw listing too; zvcs compared the
//! content without ever consulting the source.

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

/// A committed repository whose index is back-dated, so every entry is racily clean.
fn fixture(bin: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join(format!("zvcs-racyattr-{}-{}", std::process::id(), if bin == BIN { "zvcs" } else { "stock" }));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    run(bin, &dir, &[], &["init", "-q", "-b", "main", "."]);
    std::fs::write(dir.join("src/a.rs"), "a\n").unwrap();
    std::fs::write(dir.join("b.txt"), "b\n").unwrap();
    run(bin, &dir, &[], &["add", "."]);
    run(bin, &dir, &[], &["commit", "-q", "-m", "one"]);
    let index = std::fs::File::options().write(true).open(dir.join(".git/index")).unwrap();
    index.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(946_684_800)).unwrap();
    dir
}

#[test]
fn a_racily_clean_entry_dies_on_a_bad_attr_source() {
    let Some(stock) = stock_git() else { return };
    let (s, z) = (fixture(stock), fixture(BIN));
    let bad = [("GIT_ATTR_SOURCE", "does-not-exist")];
    let vectors: &[&[&str]] = &[
        &["diff-files"],
        &["diff-files", "--", "b.txt"],
        &["diff-files", "--", "no-such-path"],
        &["diff-files", ":(glob)**/*.rs"],
        &["diff-index", "HEAD"],
        &["diff-index", "HEAD", "--", "src"],
        &["diff-index", "HEAD", "--", "no-such-path"],
        &["diff-index", "--cached", "HEAD"],
    ];
    for args in vectors {
        let want = run(stock, &s, &bad, args);
        assert_eq!(run(BIN, &z, &bad, args), want, "{args:?}");
        // The same invocation without the bad source lists nothing and does not die.
        assert_eq!(run(BIN, &z, &[], args), run(stock, &s, &[], args), "{args:?} (good source)");
    }
    let _ = (std::fs::remove_dir_all(s), std::fs::remove_dir_all(z));
}
