//! `prune` removes a stale `tmp_*` *directory* as one: `prune_tmp_file()`
//! (builtin/prune.c:31-55) reports it as "Removing stale temporary directory"
//! and takes it out with `remove_dir_recursively()`. zvcs called every entry a
//! "temporary file" and `unlink()`ed it, which fails on a directory, so the
//! directory stayed behind and `count-objects -v` went on calling it garbage.
//!
//! Stock git is the oracle (`support/stock_git.rs`).

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn git(bin: &str, dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir.parent().unwrap())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    (
        out.status.code().expect("no signal"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Set a path's mtime to 2020-01-01, well past any `--expire` used here.
fn age(path: &Path) {
    let status = Command::new("touch").args(["-t", "202001010000"]).arg(path).status().unwrap();
    assert!(status.success());
}

/// A repository with stale `tmp_*` directories under `objects/` and
/// `objects/pack/`, one of them holding a file, and a stale `tmp_*` file.
fn fixture(stock: &str, label: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-prune-tmpdirs-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    let dir = std::fs::canonicalize(&dir).unwrap();
    let run = |args: &[&str]| assert_eq!(git(stock, &dir, args).0, 0, "{args:?}");
    run(&["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("f"), "f\n").unwrap();
    run(&["add", "f"]);
    run(&["commit", "-q", "-m", "f"]);
    run(&["repack", "-q", "-a", "-d"]);
    let objects = dir.join(".git/objects");
    for d in [objects.join("tmp_dir_y"), objects.join("pack/tmp_dir_x")] {
        std::fs::create_dir_all(&d).unwrap();
    }
    std::fs::write(objects.join("pack/tmp_dir_x/inside"), "j\n").unwrap();
    std::fs::write(objects.join("tmp_obj_a"), "j\n").unwrap();
    for p in ["tmp_dir_y", "pack/tmp_dir_x", "tmp_obj_a"] {
        age(&objects.join(p));
    }
    (root, dir)
}

/// What is left under `objects/` once the run is over, besides the pack.
fn leftovers(dir: &Path) -> Vec<String> {
    let objects = dir.join(".git/objects");
    let mut out = Vec::new();
    for sub in ["", "pack"] {
        for e in std::fs::read_dir(objects.join(sub)).unwrap().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with("tmp_") {
                out.push(format!("{sub}/{name}"));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn stale_tmp_directories_are_removed_whole() {
    let Some(stock) = stock_git() else { return };
    for args in [
        &["prune", "-v"][..],
        &["prune", "-n"],
        &["prune", "--expire=now", "-n", "-v"],
        &["prune"],
    ] {
        let label = args.join("");
        let (s_root, s) = fixture(stock, &format!("{label}-stock"));
        let (z_root, z) = fixture(stock, &format!("{label}-zvcs"));
        let want = git(stock, &s, args);
        let got = git(ZVCS, &z, args);
        assert_eq!(got, want, "{args:?}: left is zvcs, right is stock");
        assert_eq!(leftovers(&z), leftovers(&s), "{args:?}: what is left");
        let _ = std::fs::remove_dir_all(&s_root);
        let _ = std::fs::remove_dir_all(&z_root);
    }
}
