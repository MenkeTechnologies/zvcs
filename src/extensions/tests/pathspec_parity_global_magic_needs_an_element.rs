//! `GIT_GLOB_PATHSPECS` together with `GIT_NOGLOB_PATHSPECS` is only fatal once a pathspec
//! element is parsed: `parse_pathspec()` returns before `init_pathspec_item()` for an empty
//! list, so the contradictory settings are never looked at (pathspec.c). `ls-files` and
//! `grep` died on them regardless of whether an element existed.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
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
        "zvcs-psglobal-{}-{}",
        std::process::id(),
        if bin == BIN { "zvcs" } else { "stock" }
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    run(bin, &dir, &["init", "-q", "-b", "main", "."]);
    std::fs::write(dir.join("a"), "a\n").unwrap();
    std::fs::write(dir.join("sub/b"), "b\n").unwrap();
    run(bin, &dir, &["add", "a", "sub/b"]);
    run(bin, &dir, &["commit", "-q", "-m", "one"]);
    dir
}

#[test]
fn contradictory_global_settings_die_only_with_a_pathspec_element() {
    let Some(stock) = stock_git() else { return };
    let (s, z) = (fixture(stock), fixture(BIN));
    let flags = ["--glob-pathspecs", "--noglob-pathspecs"];
    let vectors: &[&[&str]] = &[
        &["ls-files"],
        &["ls-files", "a"],
        &["ls-files", "-o"],
        &["grep", "-l", "a"],
        &["grep", "-l", "a", "--", "sub"],
        &["grep", "-l", "--cached", "a"],
    ];
    for args in vectors {
        let full: Vec<&str> = flags.iter().chain(args.iter()).copied().collect();
        assert_eq!(run(BIN, &z, &full), run(stock, &s, &full), "{full:?}");
        // From a subdirectory the implicit prefix is not an element either.
        assert_eq!(run(BIN, &z.join("sub"), &full), run(stock, &s.join("sub"), &full), "sub {full:?}");
    }
    let _ = (std::fs::remove_dir_all(s), std::fs::remove_dir_all(z));
}
