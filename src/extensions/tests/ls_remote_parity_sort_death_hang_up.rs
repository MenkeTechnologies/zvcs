//! `git ls-remote --sort=<key>` dying in the sort, run by stock git and by zvcs
//! against one remote directory.
//!
//! `cmd_ls_remote()` sorts after `transport_get_remote_refs()` and calls
//! `transport_disconnect()` only once the listing is out (builtin/ls-remote.c:
//! 145-177), so a key that dies (`missing object <oid> for <ref>`) exits with the
//! connection open: no closing flush is written, and a protocol v0/v1
//! `upload-pack` waiting for wants reads EOF and adds `fatal: the remote end hung
//! up unexpectedly`. A v2 server ends quietly on the EOF, and a sort that
//! succeeds still disconnects normally.
//!
//! No network: the remote is a directory beside an empty repository.
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

#[test]
fn a_sort_that_dies_leaves_the_connection_open() {
    let Some(stock) = stock_git() else {
        eprintln!("skipping: no stock git");
        return;
    };
    let root = std::env::temp_dir().join(format!("zvcs-ls-remote-sort-death-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let _scratch = Scratch(root.clone());
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join(".gitconfig"), "[maintenance]\n\tauto = false\n").unwrap();
    let src = root.join("src");
    let local = root.join("local");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&local).unwrap();
    ok(stock, &src, &home, &["init", "-q", "-b", "main"]);
    ok(stock, &src, &home, &["commit", "-q", "--allow-empty", "-m", "one"]);
    ok(stock, &src, &home, &["tag", "v1"]);
    ok(stock, &local, &home, &["init", "-q"]);
    ok(stock, &local, &home, &["remote", "add", "origin", "../src"]);

    let cases: &[&[&str]] = &[
        &["-c", "protocol.version=0", "ls-remote", "--sort=subject", "../src"],
        &["-c", "protocol.version=1", "ls-remote", "--sort=objectsize"],
        &["-c", "protocol.version=0", "ls-remote", "-q", "--sort=*objectname", "origin"],
        &["-c", "protocol.version=2", "ls-remote", "--sort=subject", "../src"],
        &["-c", "protocol.version=0", "ls-remote", "--sort=refname", "../src"],
        &["-c", "protocol.version=0", "ls-remote", "--exit-code", "--sort=subject", "../src", "nomatch"],
    ];
    for args in cases {
        let seen: Vec<String> = [stock, BIN]
            .iter()
            .map(|bin| {
                let out = run(bin, &local, &home, args);
                format!(
                    "rc={:?}\n--stdout\n{}--stderr\n{}",
                    out.status.code(),
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                )
            })
            .collect();
        assert_eq!(seen[1], seen[0], "case {args:?}: zvcs (left) vs stock (right)");
    }
}
