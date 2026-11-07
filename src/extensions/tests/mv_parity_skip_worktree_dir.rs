//! `git mv` across a cone-mode sparse checkout, where a directory can exist only as
//! skip-worktree entries in the index.
//!
//! * A destination that is not on disk, lies outside the definition and holds
//!   skip-worktree entries (`empty_dir_has_sparse_contents()`) is a directory that
//!   receives each source's basename: `dst_mode = SKIP_WORKTREE_DIR`
//!   (builtin/mv.c:276-281).
//! * A source of that shape is `SKIP_WORKTREE_DIR` too and is expanded into its
//!   entries (builtin/mv.c:311-318); each one then meets the skip-worktree checks.
//! * `needs_worktree_rename()` (builtin/mv.c:41-45) renames nothing when either side
//!   is out of the cone; under `--sparse` an entry leaving the cone becomes
//!   skip-worktree and its file is unlinked, or kept and named in
//!   `advise_on_moving_dirty_path()` when dirty (builtin/mv.c:607-660), and a
//!   directory source left with no entries is removed (`remove_empty_src_dirs()`).
//! * `destination exists in the index` is asked only of an out-of-cone destination
//!   and `-f` overrides it (builtin/mv.c:489-501).
//! * An unmerged entry beside a stage-0 one under the same name is `conflicted`
//!   (`index_file_exists()`, name-hash.c:735-750).

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
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("HOME", dir)
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
    )
}

fn sh(dir: &Path, script: &str) {
    let ok = Command::new("/bin/sh").arg("-c").arg(script).current_dir(dir).status().unwrap();
    assert!(ok.success(), "{script}");
}

/// `in/x/f`, `out/y/g` and `top`, with only `in` in the cone.
fn fixture(tag: &str, stock: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-mvswd-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("in/x")).unwrap();
    std::fs::create_dir_all(dir.join("out/y")).unwrap();
    let dir = dir.canonicalize().unwrap();
    std::fs::write(dir.join("in/x/f"), "in\n").unwrap();
    std::fs::write(dir.join("out/y/g"), "out\n").unwrap();
    std::fs::write(dir.join("top"), "top\n").unwrap();
    run(stock, &dir, &["init", "-q", "-b", "main"]);
    run(stock, &dir, &["add", "."]);
    assert_eq!(run(stock, &dir, &["commit", "-qm", "one"]).2, Some(0));
    assert_eq!(run(stock, &dir, &["sparse-checkout", "set", "in"]).2, Some(0));
    dir
}

/// Every path under `dir` but `.git`, with file contents.
fn tree(dir: &Path) -> Vec<(String, Option<Vec<u8>>)> {
    fn walk(root: &Path, at: &Path, out: &mut Vec<(String, Option<Vec<u8>>)>) {
        let mut names: Vec<_> = std::fs::read_dir(at).unwrap().map(|e| e.unwrap().path()).collect();
        names.sort();
        for p in names {
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
            if rel == ".git" {
                continue;
            }
            if p.is_dir() {
                out.push((rel, None));
                walk(root, &p, out);
            } else {
                out.push((rel, Some(std::fs::read(&p).unwrap())));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out
}

#[test]
fn moves_into_and_out_of_skip_worktree_directories() {
    let Some(stock) = stock_git() else { return };
    let cases: &[(&str, &[&str])] = &[
        ("", &["mv", "top", "out/y"]),
        ("", &["mv", "--sparse", "top", "out/y"]),
        ("", &["mv", "top", "out"]),
        ("", &["mv", "-v", "--sparse", "top", "out"]),
        ("", &["mv", "--sparse", "in/x/f", "out/z"]),
        ("echo dirty >> in/x/f", &["mv", "--sparse", "in/x/f", "out/z"]),
        ("", &["mv", "out", "in/x"]),
        ("", &["mv", "--sparse", "out", "in/x"]),
        ("", &["mv", "-n", "--sparse", "out", "in/x"]),
        ("", &["mv", "-k", "out", "in/x"]),
        ("", &["mv", "in", "out"]),
        ("", &["mv", "--sparse", "in", "out"]),
        ("echo u > in/x/untracked", &["mv", "--sparse", "in", "out"]),
        ("", &["mv", "--sparse", "out/y", "in"]),
        ("", &["mv", "--sparse", "top", "out/y/g"]),
        ("", &["mv", "-f", "-v", "--sparse", "top", "out/y/g"]),
        ("", &["mv", "nothere", "out"]),
        ("", &["mv", "-k", "--sparse", "top", "out", "in/x"]),
    ];
    for (setup, args) in cases {
        let mut seen = Vec::new();
        for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
            let dir = fixture(who, stock);
            if !setup.is_empty() {
                sh(&dir, setup);
            }
            let result = run(bin, &dir, args);
            let status = run(stock, &dir, &["status", "--porcelain=v2"]);
            let files = run(stock, &dir, &["ls-files", "-s", "-t"]);
            let worktree = tree(&dir);
            let _ = std::fs::remove_dir_all(&dir);
            seen.push((result, status, files, worktree));
        }
        assert_eq!(seen[1], seen[0], "setup {setup:?} args {args:?}");
    }
}

#[test]
fn a_stage_zero_entry_beside_an_unmerged_one_is_conflicted() {
    let Some(stock) = stock_git() else { return };
    let mut seen = Vec::new();
    for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
        let dir = std::env::temp_dir().join(format!("zvcs-mvconf-{who}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        run(stock, &dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("c"), "c\n").unwrap();
        run(stock, &dir, &["add", "c"]);
        let blob = run(stock, &dir, &["rev-parse", ":c"]).0;
        let info = format!("100644 {} 1\tc\n", blob.trim());
        let mut child = Command::new(stock)
            .args(["update-index", "--index-info"])
            .current_dir(&dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        std::io::Write::write_all(child.stdin.as_mut().unwrap(), info.as_bytes()).unwrap();
        assert!(child.wait().unwrap().success());
        let result = run(bin, &dir, &["mv", "c", "c2"]);
        let files = run(stock, &dir, &["ls-files", "-s"]);
        let _ = std::fs::remove_dir_all(&dir);
        seen.push((result, files));
    }
    assert_eq!(seen[1], seen[0]);
}
