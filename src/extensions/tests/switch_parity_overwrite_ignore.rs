//! `git switch --no-overwrite-ignore`, compared with stock git.
//!
//! `opts->overwrite_ignore` is on by default (builtin/checkout.c:1818, :1897);
//! `--no-overwrite-ignore` makes `init_topts()` set `preserve_ignored` (:835) and
//! `reset_tree()` set it too unless `-f` (:767). `unpack_trees()` then sets up no
//! `o->internal.dir` (unpack-trees.c:1922-1926), so `verify_absent_1()` has no
//! exclude to let an ignored file go (:2437-2443) and `verify_clean_subdirectory()`
//! reads the directory without the standard excludes (:2382-2384): an ignored file,
//! or one inside an ignored directory, refuses the switch like an untracked one.
//! zvcs accepted the option and overwrote the ignored files anyway.

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture(PathBuf);

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn git(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> (Vec<u8>, Vec<u8>, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
        .env("GIT_EDITOR", "true")
        .output()
        .unwrap();
    (out.stdout, out.stderr, out.status.code().expect("no signal"))
}

/// `main` ignores `*.log`, `out/` and `ign/`; branch `b` tracks `build.log`, a
/// file `out` and `ign/x`, and the worktree on `main` holds ignored files at all
/// three places.
fn template(stock: &str, root: &Path) -> PathBuf {
    let work = root.join("template");
    std::fs::create_dir_all(&work).unwrap();
    let run = |args: &[&str]| {
        let (_, err, rc) = git(stock, &work, root, args);
        assert_eq!(rc, 0, "{args:?}: {}", String::from_utf8_lossy(&err));
    };
    run(&["init", "-q", "-b", "main", "."]);
    run(&["config", "maintenance.auto", "false"]);
    std::fs::write(work.join(".gitignore"), "*.log\nout/\nign/\n").unwrap();
    std::fs::write(work.join("f"), "base\n").unwrap();
    run(&["add", ".gitignore", "f"]);
    run(&["commit", "-q", "-m", "base"]);
    run(&["checkout", "-q", "-b", "b"]);
    std::fs::write(work.join("build.log"), "tracked\n").unwrap();
    std::fs::write(work.join("out"), "tracked\n").unwrap();
    std::fs::create_dir_all(work.join("ign")).unwrap();
    std::fs::write(work.join("ign/x"), "tracked\n").unwrap();
    run(&["add", "-f", "build.log", "out", "ign/x"]);
    run(&["commit", "-q", "-m", "b"]);
    run(&["checkout", "-q", "main"]);
    std::fs::write(work.join("build.log"), "ignored\n").unwrap();
    std::fs::create_dir_all(work.join("out")).unwrap();
    std::fs::write(work.join("out/j"), "junk\n").unwrap();
    std::fs::create_dir_all(work.join("ign")).unwrap();
    std::fs::write(work.join("ign/x"), "junk\n").unwrap();
    work
}

/// What a switch leaves behind, read with stock git and the filesystem.
fn post_state(stock: &str, dir: &Path, home: &Path) -> String {
    let status = git(stock, dir, home, &["status", "--porcelain", "--ignored"]);
    let head = git(stock, dir, home, &["symbolic-ref", "HEAD"]);
    format!(
        "{}{}build.log={:?}",
        String::from_utf8_lossy(&status.0),
        String::from_utf8_lossy(&head.0),
        std::fs::read_to_string(dir.join("build.log")).ok(),
    )
}

#[test]
fn switch_overwrite_ignore_matches_stock() {
    let Some(stock) = stock_git() else { return };
    let root = std::env::temp_dir().join(format!("zvcs-switch-overwrite-ignore-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let fx = Fixture(root);
    let template = template(stock, &fx.0);
    let cases: [&[&str]; 4] = [
        &["switch", "--no-overwrite-ignore", "b"],
        &["switch", "--no-overwrite-ignore", "--discard-changes", "b"],
        &["switch", "--no-overwrite-ignore", "--overwrite-ignore", "b"],
        &["switch", "--no-overwrite-ignore", "-f", "b"],
    ];
    let mut diverged = Vec::new();
    for (n, args) in cases.iter().enumerate() {
        let mut outcome = Vec::new();
        for (side, bin) in [("stock", stock), ("zvcs", BIN)] {
            let dir = fx.0.join(format!("{side}-{n}"));
            let cp = Command::new("cp").arg("-R").arg(&template).arg(&dir).status().unwrap();
            assert!(cp.success());
            let (out, err, rc) = git(bin, &dir, &fx.0, args);
            outcome.push((
                rc,
                String::from_utf8_lossy(&out).into_owned(),
                String::from_utf8_lossy(&err).into_owned(),
                post_state(stock, &dir, &fx.0),
            ));
        }
        if outcome[0] != outcome[1] {
            diverged.push(format!("{args:?}\n  stock {:?}\n  zvcs  {:?}", outcome[0], outcome[1]));
        }
    }
    assert!(diverged.is_empty(), "{}", diverged.join("\n"));
}
