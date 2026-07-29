//! `git worktree add`, pinned against stock git 2.50.1.
//!
//! The test that matters most is the last one: a worktree this binary creates has
//! to be a worktree *stock git* can work in. The administrative layout is a
//! contract between two directories — `<path>/.git` naming `worktrees/<id>`, and
//! `worktrees/<id>/gitdir` naming that gitfile back — and getting either half
//! wrong produces a directory that looks right and that neither binary can use.
//!
//! The message streams are also easy to get wrong: `Preparing worktree (…)` is on
//! stderr and is printed *before* the branch-in-use check, while `HEAD is now at
//! …` comes from the checkout and lands on stdout, so `--no-checkout` prints only
//! the first of them.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");
const STOCK: &str = "/opt/homebrew/bin/git";

fn run_with(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .expect("run binary")
}

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    run_with(BIN, dir, home, args)
}

fn git(dir: &Path, home: &Path, args: &[&str]) {
    let o = run(dir, home, args);
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
}

/// A repo with two commits and a spare branch, plus a home directory.
fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-wtadd-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    // macOS puts the temp directory behind a `/var -> /private/var` symlink, and
    // both binaries record the resolved path, so the expectations have to use it.
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    let repo = root.join("src");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &home, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("f"), "x\n").unwrap();
    git(&repo, &home, &["add", "f"]);
    git(&repo, &home, &["commit", "-q", "-m", "c1"]);
    git(&repo, &home, &["branch", "other"]);
    (root, repo, home)
}

#[test]
fn add_lays_out_the_administrative_directory() {
    let (root, repo, home) = fixture("layout");
    let o = run(&repo, &home, &["worktree", "add", "../wt", "other"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        "Preparing worktree (checking out 'other')\n",
        "the preparing line is on stderr"
    );
    assert!(
        String::from_utf8_lossy(&o.stdout).starts_with("HEAD is now at "),
        "the checkout line is on stdout: {:?}",
        String::from_utf8_lossy(&o.stdout)
    );

    let wt = root.join("wt");
    let admin = repo.join(".git/worktrees/wt");
    // The two halves of the contract, each naming the other.
    assert_eq!(
        std::fs::read_to_string(wt.join(".git")).unwrap(),
        format!("gitdir: {}\n", admin.display())
    );
    assert_eq!(
        std::fs::read_to_string(admin.join("gitdir")).unwrap().trim_end(),
        wt.join(".git").to_str().unwrap()
    );
    assert_eq!(std::fs::read_to_string(admin.join("commondir")).unwrap(), "../..\n");
    assert_eq!(std::fs::read_to_string(admin.join("HEAD")).unwrap(), "ref: refs/heads/other\n");
    for f in ["ORIG_HEAD", "index", "logs/HEAD"] {
        assert!(admin.join(f).exists(), "worktrees/wt/{f} is missing");
    }
    assert!(wt.join("f").exists(), "the worktree was not checked out");

    let _ = std::fs::remove_dir_all(&root);
}

/// A branch can be checked out in one worktree only, and the refusal names the
/// worktree that holds it — after the `Preparing worktree` line, not before.
#[test]
fn add_refuses_a_branch_that_is_already_checked_out() {
    let (root, repo, home) = fixture("inuse");
    let o = run(&repo, &home, &["worktree", "add", "../wt", "main"]);
    assert_eq!(o.status.code(), Some(128));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.starts_with("Preparing worktree (checking out 'main')\n"), "{err}");
    assert!(
        err.contains(&format!("fatal: 'main' is already used by worktree at '{}'", repo.display())),
        "{err}"
    );

    // `-b` naming an existing branch fails before anything is created.
    let o = run(&repo, &home, &["worktree", "add", "-b", "other", "../wt2"]);
    assert_eq!(o.status.code(), Some(255));
    assert!(
        String::from_utf8_lossy(&o.stderr).ends_with("fatal: a branch named 'other' already exists\n"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(!root.join("wt2").exists(), "a refused add must leave no directory behind");

    let _ = std::fs::remove_dir_all(&root);
}

/// With no `<commit-ish>`, git invents a branch named after the path's last
/// component; `--no-checkout` writes the metadata and no files, and prints only
/// the `Preparing worktree` line because there was no checkout to report.
#[test]
fn add_dwims_a_branch_name_and_honours_no_checkout() {
    let (root, repo, home) = fixture("dwim");
    let o = run(&repo, &home, &["worktree", "add", "../feature-x"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        "Preparing worktree (new branch 'feature-x')\n"
    );
    assert_eq!(
        std::fs::read_to_string(repo.join(".git/worktrees/feature-x/HEAD")).unwrap(),
        "ref: refs/heads/feature-x\n"
    );

    let o = run(&repo, &home, &["worktree", "add", "--no-checkout", "-b", "nb", "../bare-wt"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(o.stdout.is_empty(), "--no-checkout has no checkout to report");
    assert!(root.join("bare-wt/.git").exists());
    assert!(!root.join("bare-wt/f").exists(), "--no-checkout must not write files");

    let _ = std::fs::remove_dir_all(&root);
}

/// The point of the whole layout: stock git has to be able to work in a worktree
/// this binary created — see its branch, commit in it, and have the main
/// repository observe the branch move.
#[test]
fn stock_git_can_work_in_a_worktree_this_binary_created() {
    if !Path::new(STOCK).exists() {
        eprintln!("skipping: {STOCK} not installed");
        return;
    }
    let (root, repo, home) = fixture("interop");
    let o = run(&repo, &home, &["worktree", "add", "../wt", "other"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let wt = root.join("wt");

    let head = run_with(STOCK, &wt, &home, &["rev-parse", "--abbrev-ref", "HEAD"]);
    assert!(head.status.success(), "{}", String::from_utf8_lossy(&head.stderr));
    assert_eq!(String::from_utf8_lossy(&head.stdout).trim_end(), "other");

    let gitdir = run_with(STOCK, &wt, &home, &["rev-parse", "--git-dir"]);
    assert_eq!(
        String::from_utf8_lossy(&gitdir.stdout).trim_end(),
        repo.join(".git/worktrees/wt").to_str().unwrap()
    );

    std::fs::write(wt.join("z"), "z\n").unwrap();
    for args in [&["add", "z"][..], &["commit", "-q", "-m", "in the worktree"][..]] {
        let o = run_with(STOCK, &wt, &home, args);
        assert!(o.status.success(), "stock {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    }
    // The main repository sees `other` at the new commit.
    let moved = run_with(STOCK, &repo, &home, &["log", "--oneline", "-1", "other"]);
    assert!(
        String::from_utf8_lossy(&moved.stdout).contains("in the worktree"),
        "{}",
        String::from_utf8_lossy(&moved.stdout)
    );
    let fsck = run_with(STOCK, &repo, &home, &["fsck", "--no-progress"]);
    assert!(fsck.status.success(), "{}", String::from_utf8_lossy(&fsck.stderr));

    let _ = std::fs::remove_dir_all(&root);
}
