//! `git filter-branch` argument classification and `--bare`, against stock git.
//!
//! * The script has already `cd`ed into the empty scratch work tree `$tempdir/t` when it
//!   classifies `<rev-list options>`, so a bare word that names a file of the *real* work
//!   tree (`README.md`) is neither a revision nor a file there: `rev-parse` dies with
//!   `ambiguous argument` at 128 instead of the word becoming a pathspec.
//! * `--bare` points `$GIT_DIR` at a directory that is not a repository; the script, not
//!   the dispatcher, reports that.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

type Outcome = (String, String, Option<i32>);

fn run(bin: &str, dir: &Path, args: &[&str]) -> Outcome {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("LC_ALL", "C")
        .env("FILTER_BRANCH_SQUELCH_WARNING", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@x")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .unwrap();
    // macOS spells the temp directory through `/private`, which git resolves.
    let real = dir.canonicalize().unwrap();
    let text = |b: &[u8]| {
        String::from_utf8_lossy(b)
            .replace(real.to_str().unwrap(), "<R>")
            .replace(dir.to_str().unwrap(), "<R>")
    };
    (text(&out.stdout), text(&out.stderr), out.status.code())
}

fn fixture(stock: &str, root: &Path) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    let git = |args: &[&str]| run(stock, root, args);
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(root.join("README.md"), "a\n").unwrap();
    std::fs::write(root.join("src/lib.rs"), "l\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "one"]);
    std::fs::write(root.join("README.md"), "a\nb\n").unwrap();
    git(&["commit", "-qam", "two"]);
}

/// Both binaries on identical fresh repositories: same outcome, same refs afterwards.
fn same(name: &str, setup: &dyn Fn(&str, &Path), args: &[&str]) -> Outcome {
    let stock = stock_git::stock_git_at_least((2, 56, 0)).expect("caller checked");
    let base = std::env::temp_dir().join(format!("zvcs-fb-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let mut seen: Vec<(Outcome, String)> = Vec::new();
    for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
        let root = base.join(who);
        fixture(stock, &root);
        setup(stock, &root);
        let outcome = run(bin, &root, args);
        let refs = run(stock, &root, &["for-each-ref", "--format=%(refname) %(objectname)"]).0;
        seen.push((outcome, refs));
    }
    let _ = std::fs::remove_dir_all(&base);
    assert_eq!(seen[0], seen[1], "{args:?}");
    seen.remove(0).0
}

#[test]
fn a_word_of_the_real_work_tree_is_not_a_pathspec() {
    if stock_git::stock_git_at_least((2, 56, 0)).is_none() {
        return;
    }
    for args in [
        &["filter-branch", "README.md"][..],
        &["filter-branch", "--prune-empty", "main", "README.md"],
        &["filter-branch", "main", "src"],
    ] {
        let (_, stderr, code) = same("scratch", &|_, _| {}, args);
        assert_eq!(code, Some(128), "{args:?}: {stderr}");
        assert!(stderr.contains("fatal: ambiguous argument"), "{args:?}: {stderr}");
    }
    // The same words behind `--` take the pathspec arm of the script, whatever it answers.
    same("dashdash", &|_, _| {}, &["filter-branch", "--", "README.md"]);
}

#[test]
fn bare_names_a_directory_that_is_not_a_repository() {
    if stock_git::stock_git_at_least((2, 56, 0)).is_none() {
        return;
    }
    let (stdout, stderr, code) =
        same("bare", &|_, _| {}, &["--bare", "--paginate", "filter-branch", "HEAD~1..HEAD"]);
    assert_eq!((stdout.as_str(), code), ("", Some(128)), "{stderr}");
    assert_eq!(stderr, "fatal: not a git repository: '<R>'\n");
}

