//! Two `git init` details measured against stock git.
//!
//! * Re-initializing a git dir as bare rewrites `core.bare` where the assignment
//!   already stands (`git_config_set`), even when a later `[core]` block exists.
//!   zvcs appended a second `bare = true` to the last block and left the old
//!   `bare = false` in place.
//! * `real_pathdup(real_git_dir, 1)` runs ahead of the operand-count check, so an
//!   unresolvable relative `--separate-git-dir` dies (128) rather than printing
//!   the usage block (129) when a second operand is present.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::Path;
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> (String, String, Option<i32>) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", dir)
        .env("ZVCS_HOME", dir)
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    let root = dir.to_string_lossy();
    let scrub = |b: &[u8]| String::from_utf8_lossy(b).replace(root.as_ref(), "<ROOT>");
    (scrub(&out.stdout), scrub(&out.stderr), out.status.code())
}

fn fixture(tag: &str, who: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-initparity-{tag}-{who}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    assert!(Command::new(BIN).args(["init", "-q"]).current_dir(&dir).status().unwrap().success());
    dir
}

#[test]
fn bare_reinit_edits_the_existing_assignment_in_place() {
    let Some(stock) = stock_git() else { return };
    let mut configs = Vec::new();
    for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
        let dir = fixture("bare", who);
        let cfg = dir.join(".git/config");
        let mut text = std::fs::read_to_string(&cfg).unwrap();
        text.push_str("[core]\n\tprotectHFS = yes\n");
        std::fs::write(&cfg, text).unwrap();
        let result = run(bin, &dir.join(".git"), &["--bare", "init", "-q"]);
        let text = std::fs::read_to_string(&cfg).unwrap();
        configs.push((result, text));
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert_eq!(configs[1], configs[0]);
}

#[test]
fn an_unresolvable_separate_git_dir_dies_before_the_operand_count_is_judged() {
    let Some(stock) = stock_git() else { return };
    let mut results = Vec::new();
    for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
        let dir = fixture("sep", who);
        let mut per_args = Vec::new();
        for args in [
            &["init", "--separate-git-dir=nested/g", ".", "extra"][..],
            &["init", "--separate-git-dir=nested/g", "--bare", ".", "extra"],
            &["init", "--template=nested/t", ".", "extra"],
            &["init", "--separate-git-dir=", ".", "extra"],
        ] {
            per_args.push(run(bin, &dir, args));
        }
        results.push(per_args);
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert_eq!(results[1], results[0]);
}
