//! `core.ignoreStat` sets git's `assume_unchanged` global (environment.c:374), and
//! `fill_stat_cache_info()` (read-cache.c:197) then gives every entry a checkout records
//! the `CE_VALID` bit — before it looks at the file type, so symlinks too. `ls-files -v`
//! shows those entries in lower case. `add` and `update-index` already did this; the
//! commands that write the work tree out of a tree (`reset`, `checkout -f`,
//! `checkout-index -u`, `read-tree -u`, a merge, a cherry-pick) recorded the stat and left
//! the bit off.

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

/// Two commits over `a.txt`, `b.txt`, a script and a symlink; `main` is checked out at the second.
fn fixture(stock: &str, label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-ignorestat-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    let dir = std::fs::canonicalize(&dir).unwrap();
    let run = |args: &[&str]| {
        let out = git(stock, &dir, args);
        assert_eq!(out.0, 0, "{args:?}: {out:?}");
    };
    run(&["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("a.txt"), "a1\n").unwrap();
    std::fs::write(dir.join("b.txt"), "b1\n").unwrap();
    std::fs::write(dir.join("run.sh"), "#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::{symlink, PermissionsExt};
        std::fs::set_permissions(dir.join("run.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
        symlink("a.txt", dir.join("link")).unwrap();
    }
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "one"]);
    std::fs::write(dir.join("a.txt"), "a2\n").unwrap();
    std::fs::write(dir.join("c.txt"), "c2\n").unwrap();
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "two"]);
    dir
}

fn same(label: &str, steps: &[&[&str]]) {
    let Some(stock) = stock_git() else { return };
    let mut seen = Vec::new();
    for (bin, side) in [(stock, "stock"), (ZVCS, "zvcs")] {
        let dir = fixture(stock, &format!("{label}-{side}"));
        let mut runs = Vec::new();
        for step in steps {
            let mut args = vec!["-c", "core.ignoreStat=true"];
            args.extend_from_slice(step);
            runs.push(git(bin, &dir, &args));
        }
        // Read the result with the stock git, so only what the command under test wrote differs.
        let listing = git(stock, &dir, &["ls-files", "-v"]).1;
        let status = git(stock, &dir, &["status", "--porcelain"]).1;
        seen.push((runs, listing, status));
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }
    assert_eq!(seen[1], seen[0], "{label}: left is zvcs, right is stock");
}

#[test]
fn reset_marks_the_entries_it_writes() {
    same("reset-hard", &[&["reset", "--hard", "HEAD~1"]]);
    same("reset-merge", &[&["reset", "--merge", "HEAD~1"]]);
    same("reset-keep", &[&["reset", "--keep", "HEAD~1"]]);
}

#[test]
fn checkout_family_marks_the_entries_it_writes() {
    same("checkout-f", &[&["checkout", "-f", "HEAD~1"]]);
    same("checkout-b", &[&["checkout", "-b", "side", "HEAD~1"]]);
    same("switch-detach", &[&["switch", "--detach", "HEAD~1"]]);
    same("checkout-index", &[&["checkout-index", "-a", "-f", "-u"]]);
    same("read-tree", &[&["read-tree", "-u", "--reset", "HEAD~1"]]);
}

#[test]
fn merge_and_pick_mark_the_entries_they_write() {
    same(
        "merge",
        &[&["checkout", "-q", "-b", "side", "HEAD~1"], &["checkout", "-q", "main"], &["merge", "-q", "--no-edit", "side"]],
    );
    same("pick", &[&["checkout", "-q", "-b", "side", "HEAD~1"], &["cherry-pick", "main"]]);
}

#[test]
fn without_the_setting_nothing_is_marked() {
    let Some(stock) = stock_git() else { return };
    let dir = fixture(stock, "off");
    let _ = git(ZVCS, &dir, &["reset", "--hard", "HEAD~1"]);
    let listing = git(stock, &dir, &["ls-files", "-v"]).1;
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    assert!(listing.lines().all(|l| l.starts_with("H ")), "{listing}");
}
