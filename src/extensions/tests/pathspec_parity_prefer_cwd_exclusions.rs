//! An all-exclusion pathspec run from a subdirectory, against stock git.
//!
//! `parse_pathspec()` appends a `.` item when every element is an exclusion, and
//! under `PATHSPEC_PREFER_CWD` that item is the prefix, under `PATHSPEC_PREFER_FULL`
//! (or neither flag) the whole tree (pathspec.c:671-678). So `ls-files`, `rm`,
//! `grep`, `clean`, `archive` and `rerere forget` (the `PATHSPEC_PREFER_CWD`
//! callers) see only the current directory, while `add`, `commit`, `stash`,
//! `restore`, `checkout`, `diff` and `log` see everything outside the exclusion.
//!
//! Also covers `stash push` from a subdirectory: `prefix_magic()` writes the
//! prefix length `prefix_path_gently()` leaves (`deep` in `sub/` is
//! `:(prefix:4)sub/deep`), which is what the `add` child it runs at the top is
//! handed, and which `:(icase)` matching (core.ignoreCase) must take as the
//! directory `sub` and its `/`.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use std::path::Path;
use std::process::Command;

fn stock_view(stock: &str, repo: &Path, args: &[&str]) -> String {
    let out = Command::new(stock)
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
}

const FILES: &[&str] = &["t", "x", "sub/a", "sub/x", "sub/deep/b", "sub/deep/x", "other/c"];

/// Tag `tree`: `t`, `x` at the top, `sub/{a,x}`, `sub/deep/{b,x}` and `other/c`.
fn commit_tree(side: &twin_repo::Side) {
    for f in FILES {
        side.write(f, &format!("hello {f}\n"));
    }
    side.git(&["add", "."]);
    side.git(&["commit", "-q", "-m", "tree"]);
    side.git(&["tag", "tree"]);
}

/// `tree` checked out with every file but the `x`s modified, and `u`, `sub/u`,
/// `sub/xu`, `other/u` untracked.
fn fixture(side: &twin_repo::Side) {
    side.git(&["reset", "-q", "--hard", "tree"]);
    side.git(&["clean", "-qfdx"]);
    side.git(&["stash", "clear"]);
    for f in FILES.iter().filter(|f| !f.ends_with('x')) {
        side.write(f, &format!("hello {f}\nmod\n"));
    }
    for f in ["u", "sub/u", "sub/xu", "other/u"] {
        side.write(f, "u\n");
    }
}

const SPECS: &[&[&str]] = &[
    &[":!x"],
    &[":(exclude)x"],
    &[":^x"],
    &[":(top,exclude)x"],
    &[":!x", ":!deep"],
    &["deep", ":!x"],
    &["../other", ":!x"],
];

const VERBS: &[&[&str]] = &[
    &["ls-files"],
    &["ls-files", "--error-unmatch"],
    &["ls-files", "-o"],
    &["ls-files", "-m"],
    &["rm", "--cached", "-n", "-r"],
    &["rm", "--cached", "-n"],
    &["grep", "-n", "hello"],
    &["grep", "--cached", "-n", "hello"],
    &["grep", "-n", "hello", "HEAD"],
    &["clean", "-n"],
    &["clean", "-n", "-d"],
    &["archive", "--format=tar", "HEAD"],
    &["add", "--dry-run"],
    &["add", "-u", "--dry-run"],
    &["add", "-A"],
    &["diff", "--stat"],
    &["diff-index", "--name-only", "HEAD"],
    &["log", "--oneline", "--stat"],
    &["status", "--porcelain"],
    &["commit", "-q", "-m", "m", "--only"],
    &["restore"],
    &["stash", "push", "-q"],
];

fn compare(s: &twin_repo::Side, z: &twin_repo::Side, stock: &str, dir: &str, args: &[&str]) {
    for side in [s, z] {
        fixture(side);
    }
    let run = |side: &twin_repo::Side| side.run_in(&side.repo().join(dir), &[], args);
    let (zo, so) = (run(z), run(s));
    if args.contains(&"archive") {
        assert_eq!((zo.code, &zo.stderr), (so.code, &so.stderr), "in {dir}: {args:?}");
        assert!(zo.stdout == so.stdout, "in {dir}: {args:?}: the archives differ");
    } else {
        assert_eq!(zo, so, "in {dir}: {args:?}");
    }
    for view in [&["ls-files", "-s"][..], &["status", "--porcelain"], &["stash", "list"], &["log", "--format=%H %s"]] {
        assert_eq!(
            stock_view(stock, &z.repo(), view),
            stock_view(stock, &s.repo(), view),
            "in {dir}: {view:?} after {args:?}"
        );
    }
}

#[test]
fn an_all_exclusion_pathspec_follows_each_verbs_prefer_flag() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("pathspec-prefer-cwd", stock);
    commit_tree(&s);
    commit_tree(&z);
    for dir in ["sub", "sub/deep"] {
        for verb in VERBS {
            for spec in SPECS {
                let mut args: Vec<&str> = verb.to_vec();
                args.push("--");
                args.extend_from_slice(spec);
                compare(&s, &z, stock, dir, &args);
            }
        }
    }
}

#[test]
fn stash_push_from_a_subdirectory_hands_the_prefix_to_its_add_child() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("pathspec-prefer-cwd-stash", stock);
    commit_tree(&s);
    commit_tree(&z);
    for ignore_case in ["false", "true"] {
        for side in [&s, &z] {
            side.git(&["config", "core.ignoreCase", ignore_case]);
        }
        for spec in [
            &["deep"][..],
            &["a"],
            &["./deep/../a"],
            &["../sub/deep/b"],
            &["../t"],
            &["."],
            &["nosuch"],
            &[":(icase)A"],
            &[":(top)t", "deep"],
        ] {
            let mut args = vec!["stash", "push", "-q", "--"];
            args.extend_from_slice(spec);
            compare(&s, &z, stock, "sub", &args);
        }
    }
}
