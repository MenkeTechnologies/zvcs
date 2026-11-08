//! `git switch --discard-changes` without `-f`, and `switch` on an unborn branch.
//!
//! `reset_tree()` is what `--discard-changes` runs, and only `-f` makes it overwrite
//! untracked files:
//!
//! ```c
//! opts.reset = o->force ? UNPACK_RESET_OVERWRITE_UNTRACKED :
//!                         UNPACK_RESET_PROTECT_UNTRACKED;
//! ```
//!
//! (builtin/checkout.c:765-766.) Under `UNPACK_RESET_PROTECT_UNTRACKED`, `oneway_merge()`'s
//! `merged_entry()` runs `verify_absent()` for a path the index does not hold
//! (unpack-trees.c:2479-2541), so an untracked file, a directory with untracked files in it, or
//! an untracked file where the target needs a directory stops the switch with the plumbing
//! wording and exit 128. zvcs overwrote all of them.
//!
//! `parse_branchname_arg()` resolves the operand before `switch` asks whether it is already
//! there (builtin/checkout.c:1495-1539), so the unborn branch `HEAD` names is
//! `invalid reference`, where zvcs said `Already on`. `--orphan` from an unborn branch with no
//! index file reads that index as empty (read-cache.c:2232), where zvcs failed on the missing
//! commit.

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
        .env("GIT_EDITOR", "true")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    (
        out.status.code().expect("no signal"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn write(dir: &Path, rel: &str, body: &str) {
    let full = dir.join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, body).unwrap();
}

/// `main` holds `a` and `dir/c`; `side` adds `s` and `q/x`; `flat` turns `dir` into a file.
fn branches(stock: &str, dir: &Path) {
    let ok = |args: &[&str]| assert_eq!(git(stock, dir, args).0, 0, "{args:?}");
    ok(&["init", "-q", "-b", "main"]);
    write(dir, "a", "a\n");
    write(dir, "dir/c", "c\n");
    ok(&["add", "."]);
    ok(&["-c", "maintenance.auto=false", "commit", "-q", "-m", "base"]);
    ok(&["switch", "-q", "-c", "side"]);
    write(dir, "s", "s\n");
    write(dir, "q/x", "x\n");
    ok(&["add", "."]);
    ok(&["-c", "maintenance.auto=false", "commit", "-q", "-m", "side"]);
    ok(&["switch", "-q", "-c", "flat", "main"]);
    ok(&["rm", "-q", "-r", "dir"]);
    write(dir, "dir", "flat\n");
    ok(&["add", "dir"]);
    ok(&["-c", "maintenance.auto=false", "commit", "-q", "-m", "flat"]);
    ok(&["switch", "-q", "main"]);
}

fn same(label: &str, setup: &dyn Fn(&str, &Path), args: &[&str]) {
    let Some(stock) = stock_git() else { return };
    let mut seen = Vec::new();
    for (bin, side) in [(stock, "stock"), (ZVCS, "zvcs")] {
        let root = std::env::temp_dir().join(format!("zvcs-switch-discard-{label}-{side}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir: PathBuf = root.join("repo");
        std::fs::create_dir_all(&dir).unwrap();
        setup(stock, &dir);
        let run = git(bin, &dir, args);
        let listing: Vec<(String, Option<String>)> = ["a", "s", "q", "q/x", "dir", "dir/c", "q/junk", "s/junk"]
            .iter()
            .map(|p| (p.to_string(), std::fs::read_to_string(dir.join(p)).ok()))
            .collect();
        seen.push((
            run,
            std::fs::read_to_string(dir.join(".git/HEAD")).unwrap(),
            git(stock, &dir, &["ls-files", "-s"]).1,
            git(stock, &dir, &["status", "--porcelain", "-uall"]).1,
            git(stock, &dir, &["reflog", "show", "--format=%H %gs", "HEAD", "--"]).1,
            listing,
        ));
        let _ = std::fs::remove_dir_all(&root);
    }
    assert_eq!(seen[1], seen[0], "git {args:?}: left is zvcs, right is stock");
}

#[test]
fn discard_changes_protects_what_is_untracked() {
    let file = |stock: &str, dir: &Path| {
        branches(stock, dir);
        write(dir, "s", "untracked\n");
    };
    same("file", &file, &["switch", "--discard-changes", "side"]);
    same("file-create", &file, &["switch", "--discard-changes", "-c", "x", "side"]);
    same("file-detach", &file, &["switch", "--discard-changes", "--detach", "side"]);
    same("file-force", &file, &["switch", "-f", "side"]);
    let ignored = |stock: &str, dir: &Path| {
        file(stock, dir);
        write(dir, ".gitignore", "s\n");
    };
    same("ignored", &ignored, &["switch", "--discard-changes", "side"]);
    let directory = |stock: &str, dir: &Path| {
        branches(stock, dir);
        write(dir, "s/junk", "junk\n");
    };
    same("directory", &directory, &["switch", "--discard-changes", "side"]);
    let empty_directory = |stock: &str, dir: &Path| {
        branches(stock, dir);
        std::fs::create_dir_all(dir.join("s/e")).unwrap();
    };
    same("empty-directory", &empty_directory, &["switch", "--discard-changes", "side"]);
    let leading = |stock: &str, dir: &Path| {
        branches(stock, dir);
        write(dir, "q", "in the way\n");
    };
    same("leading", &leading, &["switch", "--discard-changes", "side"]);
    same("leading-force", &leading, &["switch", "-f", "side"]);
    // `dir/c` is tracked, so the file `flat` needs at `dir` replaces a directory the index
    // holds entries under, and a stale untracked file there is protected too.
    let replaced = |stock: &str, dir: &Path| {
        branches(stock, dir);
        write(dir, "dir/junk", "junk\n");
    };
    same("replaced", &replaced, &["switch", "--discard-changes", "flat"]);
    same("replaced-clean", &branches, &["switch", "--discard-changes", "flat"]);
}

#[test]
fn an_unborn_branch_is_no_branch_to_be_on() {
    let unborn = |stock: &str, dir: &Path| {
        assert_eq!(git(stock, dir, &["init", "-q", "-b", "main"]).0, 0);
        write(dir, "a", "a\n");
    };
    same("unborn-same", &unborn, &["switch", "main"]);
    same("unborn-orphan", &unborn, &["switch", "--orphan", "o"]);
    same("unborn-create", &unborn, &["switch", "-c", "o"]);
    let staged = |stock: &str, dir: &Path| {
        unborn(stock, dir);
        assert_eq!(git(stock, dir, &["add", "a"]).0, 0);
    };
    same("staged-same", &staged, &["switch", "main"]);
    same("staged-orphan", &staged, &["switch", "--orphan", "o"]);
}
