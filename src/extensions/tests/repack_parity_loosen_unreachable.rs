//! `repack -A` and `--unpack-unreachable[=<date>]` turn the unreachable objects of the packs `-d`
//! deletes back into loose objects (`loosen_unused_packed_objects()`, pack-objects.c), dated with
//! their pack; with a date, one whose pack is no newer than it and which no recent object reaches
//! is dropped instead. `-k -d` packs the loose unreachable objects (`--pack-loose-unreachable`)
//! even when there is no pack to delete, and the unreachable objects of the packs there are only
//! when there is one (`--keep-unreachable`).
//!
//! zvcs deleted the superseded packs and with them every unreachable object they held, and left
//! the loose unreachable ones loose under `-k` unless an old pack existed — so `gc --no-cruft`
//! after a cruft `gc` lost objects stock keeps.

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

/// `main` at one commit, plus a second commit on a deleted branch: three unreachable loose objects.
fn fixture(stock: &str, label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-loosen-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    let dir = std::fs::canonicalize(&dir).unwrap();
    let run = |args: &[&str]| {
        let out = git(stock, &dir, args);
        assert_eq!(out.0, 0, "{args:?}: {out:?}");
    };
    run(&["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("f"), "one\n").unwrap();
    run(&["add", "f"]);
    run(&["commit", "-q", "-m", "one"]);
    run(&["checkout", "-q", "-b", "side"]);
    std::fs::write(dir.join("f"), "two\n").unwrap();
    run(&["commit", "-q", "-am", "two"]);
    run(&["checkout", "-q", "main"]);
    run(&["branch", "-q", "-D", "side"]);
    run(&["reflog", "expire", "--expire=now", "--all"]);
    dir
}

/// Where every object ended up: the `count-objects -v` counters and the sorted loose ids.
fn shape(stock: &str, dir: &Path) -> (String, Vec<String>) {
    let counters: String = git(stock, dir, &["count-objects", "-v"])
        .1
        .lines()
        .filter(|l| l.starts_with("count:") || l.starts_with("in-pack:") || l.starts_with("packs:"))
        .collect::<Vec<_>>()
        .join(" ");
    let mut loose = Vec::new();
    for fan in std::fs::read_dir(dir.join(".git/objects")).unwrap().flatten() {
        let name = fan.file_name().into_string().unwrap();
        if name.len() == 2 && name.bytes().all(|b| b.is_ascii_hexdigit()) {
            for f in std::fs::read_dir(fan.path()).unwrap().flatten() {
                loose.push(format!("{name}{}", f.file_name().into_string().unwrap()));
            }
        }
    }
    loose.sort();
    (counters, loose)
}

fn same(label: &str, steps: &[&[&str]]) {
    let Some(stock) = stock_git() else { return };
    let mut seen = Vec::new();
    for (bin, side) in [(stock, "stock"), (ZVCS, "zvcs")] {
        let dir = fixture(stock, &format!("{label}-{side}"));
        let mut codes = Vec::new();
        for step in steps {
            codes.push(git(bin, &dir, step).0);
        }
        seen.push((codes, shape(stock, &dir)));
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }
    assert_eq!(seen[1], seen[0], "{label}: left is zvcs, right is stock");
}

#[test]
fn keep_unreachable_packs_the_loose_unreachable_objects_even_with_no_pack_to_delete() {
    same("k-first", &[&["repack", "-q", "-a", "-d", "-k"]]);
    same("k-twice", &[&["repack", "-q", "-a", "-d", "-k"], &["repack", "-q", "-a", "-d", "-k"]]);
    same("k-without-d", &[&["repack", "-q", "-a", "-k"]]);
}

#[test]
fn a_capital_a_loosens_the_unreachable_objects_of_the_packs_it_deletes() {
    same("A", &[&["repack", "-q", "-a", "-d", "-k"], &["repack", "-q", "-A", "-d"]]);
    same("A-nonexistent-d", &[&["repack", "-q", "-a", "-d", "-k"], &["repack", "-q", "-A"]]);
}

#[test]
fn a_date_decides_which_of_them_are_dropped() {
    same("now", &[&["repack", "-q", "-a", "-d", "-k"], &["repack", "-q", "-A", "-d", "--unpack-unreachable=now"]]);
    same(
        "weeks",
        &[&["repack", "-q", "-a", "-d", "-k"], &["repack", "-q", "-A", "-d", "--unpack-unreachable=2.weeks.ago"]],
    );
    same("garbage", &[&["repack", "-q", "-a", "-d", "-k"], &["repack", "-q", "-A", "-d", "--unpack-unreachable=bogus"]]);
}

#[test]
fn gc_without_cruft_keeps_what_a_cruft_gc_set_aside() {
    same("gc-no-cruft", &[&["gc", "-q"], &["gc", "-q", "--no-cruft"]]);
    same("gc-prune-now", &[&["gc", "-q"], &["gc", "-q", "--prune=now", "--no-cruft"]]);
    same("gc-prune-date", &[&["gc", "-q"], &["gc", "-q", "--prune=15", "--no-cruft"]]);
    same("gc-prune-blank", &[&["gc", "-q"], &["gc", "-q", "--prune= 1", "--no-cruft"]]);
}
