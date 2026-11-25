//! The `error: failed to push some refs to ...` line of `git push` under
//! `color.push`, run by stock git and by zvcs on twin copies of one fixture at
//! the same path. `push_with_options()` wraps it in
//! `push_get_color(PUSH_COLOR_ERROR)` and `PUSH_COLOR_RESET` whatever ended the
//! push and whatever the output format (builtin/push.c:395-398): `--porcelain`, a
//! source that matches nothing and a refused refspec list included.
//!
//! No network: the remotes are directories beside the clone.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", home.join(".gitconfig"))
        .env_remove("XDG_CONFIG_HOME")
        .env("GIT_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .env("TERM", "dumb")
        .output()
        .expect("spawn git")
}

fn ok(bin: &str, dir: &Path, home: &Path, args: &[&str]) {
    let out = run(bin, dir, home, args);
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// `up` (main) cloned into `cl`, with its remote spelled `../up`, and into
/// `bare.git`; `cl` and `up` then each commit, so `cl`'s `main` is refused.
fn base(stock: &str, root: &Path, home: &Path) -> PathBuf {
    let base = root.join("base");
    let up = base.join("up");
    std::fs::create_dir_all(&up).unwrap();
    let g = |dir: &Path, args: &[&str]| ok(stock, dir, home, args);
    g(&up, &["init", "-q", "-b", "main"]);
    g(&up, &["commit", "-q", "--allow-empty", "-m", "one"]);
    g(&base, &["clone", "-q", "up", "cl"]);
    g(&base, &["clone", "-q", "--bare", "up", "bare.git"]);
    let cl = base.join("cl");
    g(&cl, &["config", "remote.origin.url", "../up"]);
    g(&cl, &["commit", "-q", "--allow-empty", "-m", "local"]);
    g(&up, &["commit", "-q", "--allow-empty", "-m", "two"]);
    base
}
/// Everything a case can change, read back with stock git.
fn state(stock: &str, cl: &Path, home: &Path) -> String {
    let refs = run(stock, cl, home, &["for-each-ref", "--format=%(refname) %(objectname)"]);
    let config = run(stock, cl, home, &["config", "--local", "--list"]);
    let fetch_head = std::fs::read_to_string(cl.join(".git/FETCH_HEAD")).unwrap_or_default();
    format!(
        "{}--\n{}--\n{fetch_head}",
        String::from_utf8_lossy(&refs.stdout),
        String::from_utf8_lossy(&config.stdout)
    )
}

/// Runs `cases` (`sh -c` scripts with `$G` naming the binary) from `cl` under
/// both binaries, each on a fresh copy of the fixture at the same path.
fn compare(tag: &str, cases: &[&str]) {
    let Some(stock) = stock_git() else {
        eprintln!("skipping: no stock git");
        return;
    };
    let root = std::env::temp_dir().join(format!("zvcs-pushcolorsweep-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let _scratch = Scratch(root.clone());
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join(".gitconfig"), "[maintenance]\n\tauto = false\n[gc]\n\tauto = 0\n").unwrap();
    let base = base(stock, &root, &home);
    let work = root.join("w");
    let cl = work.join("cl");
    for case in cases {
        let mut seen = Vec::new();
        for bin in [stock, BIN] {
            let _ = std::fs::remove_dir_all(&work);
            let cp = Command::new("cp").arg("-R").arg(&base).arg(&work).status().unwrap();
            assert!(cp.success());
            let out = Command::new("sh")
                .arg("-c")
                .arg(case)
                .current_dir(&cl)
                .env("G", bin)
                .env("S", stock)
                .env("HOME", &home)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", home.join(".gitconfig"))
                .env_remove("XDG_CONFIG_HOME")
                .env("GIT_EDITOR", "true")
                .env("GIT_AUTHOR_NAME", "A U Thor")
                .env("GIT_AUTHOR_EMAIL", "author@example.com")
                .env("GIT_COMMITTER_NAME", "C O Mitter")
                .env("GIT_COMMITTER_EMAIL", "committer@example.com")
                .env("GIT_AUTHOR_DATE", "1700000100 +0000")
                .env("GIT_COMMITTER_DATE", "1700000100 +0000")
                .env("LC_ALL", "C")
                .env("TERM", "dumb")
                .output()
                .unwrap();
            seen.push(format!(
                "rc={:?}\n--stdout\n{}--stderr\n{}--state\n{}",
                out.status.code(),
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr),
                state(stock, &cl, &home)
            ));
        }
        assert_eq!(seen[1], seen[0], "case {case:?}: zvcs (left) vs stock (right)");
    }
}

#[test]
fn the_failure_line_takes_the_error_color() {
    compare(
        "color",
        &[
            "$G -c color.push=always push --porcelain origin main",
            "$G -c color.push=always push ../bare.git nosuch",
            "$G -c color.push=always push ../bare.git main:refs/heads/x main:refs/heads/x",
            "$G -c color.push=always -c color.push.error=blue push origin main",
        ],
    );
}
