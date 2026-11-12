//! `git mv` against stock git: the order the checking loop tests a directory
//! source in, and which of its refusals `-k` can skip.
//!
//! builtin/mv.c refuses a directory whose destination `lstat()`s (a dangling
//! symlink included) with `destination already exists` (:355-359) *before*
//! `dir_check:`, so before `source directory is empty` (:371-375) and before
//! submodule_gitfile_path() looks at `.gitmodules` (:109-112). That function
//! die()s outright, so `-k`, which only drops the `bad` reasons at
//! act_on_entry: (:524-528), cannot skip a submodule move while `.gitmodules`
//! has unstaged changes. zvcs tested the submodule's `.gitmodules` first, the
//! destination only after the empty-directory check, followed symlinks, and
//! let `-k` swallow the die.
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
        .env("GIT_EDITOR", "true")
        .env("LC_ALL", "C")
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

/// A tracked file `a`, a tracked directory `dir`, an untracked directory
/// `emptyd`, and a submodule `sub` (an embedded repository behind a gitlink and
/// a committed `.gitmodules`) whose `.gitmodules` then gains an unstaged line.
fn fixture(stock: &str, root: &Path) {
    let git = |args: &[&str]| run(stock, root, args);
    std::fs::create_dir_all(root.join("dir")).unwrap();
    std::fs::create_dir_all(root.join("emptyd")).unwrap();
    std::fs::create_dir_all(root.join("sub")).unwrap();
    git(&["init", "-q", "-b", "main"]);
    run(stock, &root.join("sub"), &["init", "-q", "-b", "main"]);
    run(stock, &root.join("sub"), &["-c", "maintenance.auto=false", "commit", "-q", "--allow-empty", "-m", "s"]);
    std::fs::write(root.join("a"), "a\n").unwrap();
    std::fs::write(root.join("dir/x"), "x\n").unwrap();
    std::fs::write(root.join(".gitmodules"), "[submodule \"sub\"]\n\tpath = sub\n\turl = ./sub\n").unwrap();
    std::os::unix::fs::symlink("nowhere", root.join("dang")).unwrap();
    git(&["-c", "advice.addEmbeddedRepo=false", "add", "a", "dir", "sub", ".gitmodules"]);
    git(&["-c", "maintenance.auto=false", "commit", "-q", "-m", "one"]);
    std::fs::write(root.join(".gitmodules"), "[submodule \"sub\"]\n\tpath = sub\n\turl = ./sub\n\tx\n").unwrap();
}

#[test]
fn directory_destination_is_tested_first_and_the_gitmodules_die_survives_k() {
    let Some(stock) = stock_git::stock_git() else { return };
    let base = std::env::temp_dir().join(format!("zvcs-mv-dir-check-order-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (s, z) = (base.join("stock"), base.join("zvcs"));
    let cases: &[&[&str]] = &[
        &["mv", "sub", "a"],
        &["mv", "-k", "sub", "a"],
        &["mv", "-k", "sub", "sub2"],
        &["mv", "-k", "a", "sub", "dir"],
        &["mv", "emptyd", "a"],
        &["mv", "dir", "dang"],
        &["mv", "-k", "dir", "dang"],
    ];
    for args in cases {
        for root in [&s, &z] {
            let _ = std::fs::remove_dir_all(root);
            fixture(stock, root);
        }
        assert_eq!(run(BIN, &z, args), run(stock, &s, args), "{args:?}");
        for state in [&["ls-files", "-s"][..], &["status", "--porcelain"], &["cat-file", "blob", ":.gitmodules"]] {
            assert_eq!(run(stock, &z, state), run(stock, &s, state), "{state:?} after {args:?}");
        }
    }
    let _ = std::fs::remove_dir_all(&base);
}
