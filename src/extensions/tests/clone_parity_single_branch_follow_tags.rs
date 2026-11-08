//! What a single-branch clone fetches, and the tags it ends up with.
//!
//! `cmd_clone()` sets `TRANS_OPT_FOLLOWTAGS` for every single-branch clone and clears
//! `opts.wants_head` when `--branch` names the ref (builtin/clone.c:1380-1385), so the
//! request wants that one ref with `include-tag`, and upload-pack adds only the tags
//! pointing into the pack. `write_followtags()` then writes the advertised tags the clone
//! has, behind `option_tags` (builtin/clone.c:500-514, :556-557): a `--no-tags` or
//! `--mirror` single-branch clone receives the tag objects and writes no ref for them.
//! A `--mirror` single-branch clone fetches its one ref like any other and still records
//! `+refs/*:refs/*` (builtin/clone.c:801-835); `--branch <tag>` maps that tag twice there,
//! which the initial transaction refuses.
//!
//! Each case clones the same source with both binaries over a local path, `--no-local`,
//! `file://` and `file://` under protocol v0, and compares the output, the refs, the
//! remote and branch configuration, the objects the clone holds, `shallow` and `fsck`.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn git(bin: &str, cwd: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output()
        .expect("git runs")
}

fn ok(bin: &str, cwd: &Path, home: &Path, args: &[&str]) {
    let out = git(bin, cwd, home, args);
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// `src`: `main` at a, b, d and `feature` forked at a with c; a lightweight and an annotated
/// tag on a (reached by both branches), on c (`feature` only) and on d (`main` only).
fn source(root: &Path, stock: &str) {
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let run = |args: &[&str]| ok(stock, &src, root, args);
    let commit = |name: &str| {
        std::fs::write(src.join(name), format!("{name}\n")).unwrap();
        run(&["add", name]);
        run(&["-c", "maintenance.auto=false", "commit", "-qm", name]);
    };
    run(&["init", "-q", "-b", "main"]);
    commit("a");
    run(&["tag", "v1"]);
    run(&["tag", "-a", "-m", "on a", "v1a"]);
    commit("b");
    run(&["checkout", "-q", "-b", "feature", "HEAD~1"]);
    commit("c");
    run(&["tag", "v2"]);
    run(&["tag", "-a", "-m", "on c", "v2a"]);
    run(&["checkout", "-q", "main"]);
    commit("d");
    run(&["tag", "v3"]);
    run(&["tag", "-a", "-m", "on d", "v3a"]);
}

/// Clone `source` with `bin` into a fresh directory below `root` and report what the
/// comparison covers.
fn clone(bin: &str, stock: &str, root: &Path, side: &str, source: &[String], args: &[&str]) -> String {
    let home = root.join(side);
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let mut argv: Vec<&str> = vec!["-c", "maintenance.auto=false", "clone"];
    argv.extend_from_slice(args);
    argv.extend(source.iter().map(String::as_str));
    argv.push("dst");
    let out = git(bin, &home, &home, &argv);
    let dst = home.join("dst");
    let git_dir = if dst.join(".git").is_dir() { dst.join(".git") } else { dst.clone() };
    let read = |args: &[&str]| {
        if !git_dir.is_dir() {
            return String::new();
        }
        let out = git(stock, &git_dir, &home, args);
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
    };
    let mut objects: Vec<String> = read(&["rev-list", "--objects", "--all"]).lines().map(str::to_owned).collect();
    objects.sort();
    // Every object in the clone's packs, the tags `include-tag` brought without a ref among them.
    let mut packed: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(git_dir.join("objects").join("pack")) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "idx") {
                let out = Command::new(stock)
                    .arg("show-index")
                    .stdin(std::fs::File::open(&path).unwrap())
                    .output()
                    .expect("show-index runs");
                packed.extend(
                    String::from_utf8_lossy(&out.stdout)
                        .lines()
                        .filter_map(|l| l.split_whitespace().nth(1).map(str::to_owned)),
                );
            }
        }
    }
    packed.sort();
    let report = format!(
        "exit={}\n--- stdout\n{}--- stderr\n{}--- for-each-ref\n{}--- config\n{}--- objects\n{}\n--- packed\n{}\n--- shallow\n{}--- fsck\n{}",
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
        read(&["for-each-ref", "--format=%(refname) %(objectname) %(symref)"]),
        read(&["config", "--local", "--get-regexp", r"^(remote|branch)\."]),
        objects.join("\n"),
        packed.join("\n"),
        std::fs::read_to_string(git_dir.join("shallow")).unwrap_or_default(),
        read(&["fsck", "--no-progress", "--no-dangling"]),
    );
    report.replace(&home.to_string_lossy().into_owned(), "<home>")
}

/// Clone `src`, spelled as a path, with `--no-local`, as a `file://` url and, with `v0`, as one
/// under protocol v0, with every option set in `options` through both binaries, and compare.
fn compare(tag: &str, v0: bool, options: &[&[&str]]) {
    let Some(stock) = stock_git::stock_git() else { return };
    let root: PathBuf =
        std::env::temp_dir().join(format!("zvcs-clone-single-branch-tags-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    source(&root, stock);
    let src = root.join("src").to_string_lossy().into_owned();
    let url = format!("file://{src}");
    let mut spellings: Vec<Vec<String>> =
        vec![vec![src.clone()], vec!["--no-local".into(), src.clone()], vec![url.clone()]];
    if v0 {
        spellings.push(vec!["-c".into(), "protocol.version=0".into(), url.clone()]);
    }

    let mut differing = Vec::new();
    let mut total = 0;
    for args in options {
        for source in &spellings {
            total += 1;
            let want = clone(stock, stock, &root, "stock", source, args);
            let got = clone(ZVCS, stock, &root, "zvcs", source, args);
            if got != want {
                let source = source.join(" ").replace(&root.to_string_lossy().into_owned(), "<root>");
                differing.push(format!("clone {args:?} {source}\n--- stock\n{want}\n--- zvcs\n{got}"));
            }
        }
    }
    assert!(differing.is_empty(), "{} of {total} clones differ:\n{}", differing.len(), differing.join("\n"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_branch_clone_follows_only_the_tags_on_that_branch() {
    compare("branch", true, &[
        &["-b", "feature", "--single-branch"],
        &["-b", "v2", "--single-branch"],
        &["-b", "v2a", "--single-branch"],
        &["--depth", "1", "-b", "feature"],
        &["--depth", "2", "-b", "feature"],
        &["--bare", "--single-branch", "-b", "feature"],
        &["--single-branch"],
    ]);
}

#[test]
fn a_single_branch_clone_without_option_tags_still_asks_with_include_tag() {
    compare("no-tags", true, &[
        &["--no-tags", "--single-branch"],
        &["--no-tags", "--single-branch", "-b", "feature"],
        &["--no-tags", "--depth", "1"],
        &["--revision", "refs/heads/feature", "--depth", "1"],
        &["--revision", "refs/heads/feature", "--single-branch"],
    ]);
}

#[test]
fn a_mirror_single_branch_clone_fetches_its_one_ref() {
    compare("mirror", true, &[
        &["--mirror", "--single-branch", "-b", "feature"],
        &["--mirror", "--single-branch"],
        &["--mirror", "--depth", "1"],
        &["--mirror", "--single-branch", "-b", "v2"],
    ]);
}

/// A `--branch` the remote does not have maps nothing, and `cmd_clone()` dies of it naming the
/// remote (builtin/clone.c:459-462, :1542-1546). Not under protocol v0, where stock's
/// `upload-pack` also reports the EOF the dying client leaves it, which this port's client
/// does not provoke (it ends the interaction with a flush-packet as it drops the connection).
#[test]
fn a_single_branch_clone_of_a_missing_branch_dies_naming_the_remote() {
    compare("missing", false, &[
        &["-b", "nope", "--single-branch"],
        &["--mirror", "--single-branch", "-b", "nope"],
        &["--bare", "--single-branch", "-b", "nope"],
    ]);
}
