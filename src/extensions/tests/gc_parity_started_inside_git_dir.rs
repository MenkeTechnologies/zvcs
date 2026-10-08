//! A `gc` started inside the git directory is a bare-style discovery:
//! `setup_bare_git_dir()` chdirs to the git directory first. `pack-refs --prune` then removes
//! the emptied `refs/heads` the process was started in, and the run goes on to repack and write
//! the commit-graph. Without the chdir the later steps had no working directory and `gc` exited
//! 1 having packed nothing.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> (String, String, i32) {
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
        .env("LC_ALL", "C")
        .output()
        .expect("run git");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

fn fixture(bin: &str, tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "zvcs-gc-in-gitdir-{tag}-{}-{}",
        std::process::id(),
        if bin == BIN { "zvcs" } else { "stock" }
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    run(bin, &dir, &["init", "-q", "-b", "main", "."]);
    for (name, msg) in [("a", "one"), ("b", "two")] {
        std::fs::write(dir.join(name), format!("{name}\n")).unwrap();
        run(bin, &dir, &["add", name]);
        run(bin, &dir, &["commit", "-q", "-m", msg]);
    }
    dir
}

fn listing(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for sub in [".git", ".git/objects/info", ".git/objects/pack"] {
        let mut names: Vec<String> = std::fs::read_dir(dir.join(sub))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        out.push(format!("{sub}: {}", names.join(" ")));
    }
    out
}

#[test]
fn gc_from_refs_heads_completes() {
    let Some(stock) = stock_git() else { return };
    for (sub, args) in [
        (".git/refs/heads", &["gc", "--no-cruft"][..]),
        (".git/refs", &["gc", "--quiet"][..]),
        (".git/objects", &["gc", "--quiet"][..]),
    ] {
        let (s, z) = (fixture(stock, sub), fixture(BIN, sub));
        let want = run(stock, &s.join(sub), args);
        let got = run(BIN, &z.join(sub), args);
        assert_eq!(got, want, "{sub}: {args:?}");
        assert_eq!(listing(&z), listing(&s), "{sub}: {args:?}: store after gc");
        let _ = (std::fs::remove_dir_all(s), std::fs::remove_dir_all(z));
    }
}
