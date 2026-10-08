//! A command that rewrites an index through `unpack_trees()` keeps the version of the index it
//! read: `o->internal.result.version = o->src_index->version` (unpack-trees.c:1940).
//! `index.version` and `feature.manyFiles` only choose a version for an index that was never on
//! disk, so a version 2 index stays version 2 across `reset`, `checkout -f`, `merge --abort` and
//! the like, in stock git and here.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> Option<i32> {
    Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_VERSION")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@x")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .unwrap()
        .status
        .code()
}

/// `main` has an unresolved merge of `t` in progress, the working file `g` is dirty, and the
/// repository asks for a different index version than the one on disk (2).
fn fixture(root: &Path, setting: (&str, &str)) {
    std::fs::create_dir_all(root).unwrap();
    let git = |args: &[&str]| run(stock_git::stock_git().unwrap(), root, args);
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(root.join("f"), "base\n").unwrap();
    std::fs::write(root.join("g"), "x\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "base"]);
    git(&["checkout", "-qb", "t"]);
    std::fs::write(root.join("f"), "t\n").unwrap();
    git(&["commit", "-qam", "t"]);
    git(&["checkout", "-q", "main"]);
    std::fs::write(root.join("f"), "m\n").unwrap();
    git(&["commit", "-qam", "m"]);
    git(&["merge", "t"]);
    std::fs::write(root.join("g"), "dirty\n").unwrap();
    git(&["config", setting.0, setting.1]);
}

fn index_version(root: &Path) -> u8 {
    std::fs::read(root.join(".git/index")).unwrap()[7]
}

#[test]
fn the_source_index_version_survives_the_rewrite() {
    let Some(stock) = stock_git::stock_git() else { return };
    let base = std::env::temp_dir().join(format!("zvcs-index-version-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let settings = [("index.version", "4"), ("feature.manyFiles", "true")];
    let commands: [&[&str]; 8] = [
        &["reset", "--hard"],
        &["reset", "--mixed", "t"],
        &["reset", "--merge"],
        &["merge", "--abort"],
        &["checkout", "-f", "t"],
        &["checkout", "-f", "main"],
        &["checkout", "t"],
        &["read-tree", "-m", "-u", "HEAD", "t"],
    ];
    for setting in settings {
        for cmd in commands {
            let (s, z) = (base.join("s"), base.join("z"));
            let _ = std::fs::remove_dir_all(&base);
            fixture(&s, setting);
            fixture(&z, setting);
            let want_rc = run(stock, &s, cmd);
            let got_rc = run(BIN, &z, cmd);
            assert_eq!(got_rc, want_rc, "{cmd:?} under {setting:?}: exit status");
            assert_eq!(index_version(&z), index_version(&s), "{cmd:?} under {setting:?}: index version");
            assert_eq!(index_version(&s), 2, "{cmd:?} under {setting:?}: stock changed the version");
        }
    }
    let _ = std::fs::remove_dir_all(&base);
}
