//! A missing alternate object directory, named by the `RUN_SETUP_GENTLY` verbs.
//!
//! A verb that sets up gently and finds a repository opens its object database like any other,
//! and `odb_is_source_usable()` reports the alternate it cannot use: `error: object directory
//! <path> does not exist; check .git/objects/info/alternates`. The verbs that never reach setup
//! (`help`, `check-ref-format`, `version`, …) stay silent, and so does a gentle verb outside any
//! repository. zvcs reported it for none of the gentle ones.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, nosuch: &Path, args: &[&str]) -> (String, String, Option<i32>) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", nosuch)
        .env("GIT_CEILING_DIRECTORIES", dir.parent().unwrap())
        .env("HOME", dir)
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    let root = dir.to_string_lossy();
    let scrub = |b: &[u8]| String::from_utf8_lossy(b).replace(root.as_ref(), "<ROOT>");
    (scrub(&out.stdout), scrub(&out.stderr), out.status.code())
}

fn fresh(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-altgentle-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

/// Only the alternate diagnostics, so a verb's own output (which this test does not judge) does
/// not matter.
fn alternate_lines(r: &(String, String, Option<i32>)) -> Vec<String> {
    r.1.lines().filter(|l| l.contains("object directory")).map(str::to_owned).collect()
}

#[test]
fn gentle_verbs_name_the_missing_alternate_only_inside_a_repository() {
    let Some(stock) = stock_git() else { return };
    let verbs: [&[&str]; 12] = [
        &["config", "-l"],
        &["diff"],
        &["hash-object", "--stdin"],
        &["interpret-trailers", "--parse"],
        &["hook", "list", "pre-commit"],
        &["column"],
        &["var", "GIT_AUTHOR_IDENT"],
        &["apply", "no-such-patch"],
        &["help", "-a"],
        &["check-ref-format", "a/b"],
        &["get-tar-commit-id"],
        &["version"],
    ];
    for inside in [true, false] {
        let mut seen = Vec::new();
        for who in ["stock", "zvcs"] {
            let bin = if who == "stock" { stock } else { BIN };
            let dir = fresh(&format!("{inside}-{who}"));
            if inside {
                assert!(Command::new(stock).args(["init", "-q"]).current_dir(&dir).status().unwrap().success());
            }
            let nosuch = dir.join("no-such-objects");
            let lines: Vec<_> = verbs.iter().map(|args| alternate_lines(&run(bin, &dir, &nosuch, args))).collect();
            let _ = std::fs::remove_dir_all(&dir);
            seen.push(lines);
        }
        assert_eq!(seen[1], seen[0], "inside a repository: {inside}");
    }
}
