//! `git merge-tree --stdin` batch-mode parity.
//!
//! `--stdin` runs one merge per input line and frames each result as
//! `<clean>\n<tree-oid>\n<conflict info><messages>\n` (git's
//! `printf("%d%c", result.clean, term)` + the normal single-merge body +
//! a closing `putchar(term)`). Because zvcs and stock git operate on the same
//! object database, the tree ids are identical, so the whole byte stream and
//! the exit code can be pinned against the system `git`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git_env(cmd: &mut Command, dir: &Path, home: &Path) {
    cmd.current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CEILING_DIRECTORIES", dir)
        .env("ZVCS_HOME", home)
        .env("LC_ALL", "C")
        .env("TZ", "UTC");
}

/// Run stock `git <args>` in the fixture, asserting success (fixture setup).
fn git(dir: &Path, home: &Path, args: &[&str]) {
    let mut c = Command::new("git");
    git_env(&mut c, dir, home);
    assert!(c.args(args).status().unwrap().success(), "git {args:?}");
}

/// Run `<bin> merge-tree <args>` feeding `input` on stdin.
fn merge_tree(bin: &str, dir: &Path, home: &Path, args: &[&str], input: &str) -> Output {
    let mut c = Command::new(bin);
    git_env(&mut c, dir, home);
    let mut child = c
        .args(std::iter::once(&"merge-tree").chain(args.iter()).copied().collect::<Vec<_>>())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

/// Build a repo with a base commit and three branches off it:
///   * `main`  — three-line file
///   * `ours`  — edits line 1
///   * `far`   — edits line 3 (merges cleanly with `ours`)
///   * `clash` — edits line 1 differently (conflicts with `ours`)
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-mtstdin-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();

    git(&repo, &home, &["init", "-q", "-b", "main"]);
    git(&repo, &home, &["config", "user.email", "a@example.com"]);
    git(&repo, &home, &["config", "user.name", "A"]);

    std::fs::write(repo.join("f.txt"), "one\ntwo\nthree\n").unwrap();
    git(&repo, &home, &["add", "f.txt"]);
    git(&repo, &home, &["commit", "-q", "-m", "base"]);

    let write_commit = |branch: &str, from: &str, contents: &str, msg: &str| {
        git(&repo, &home, &["checkout", "-q", "-b", branch, from]);
        std::fs::write(repo.join("f.txt"), contents).unwrap();
        git(&repo, &home, &["commit", "-q", "-am", msg]);
    };
    write_commit("ours", "main", "ONE\ntwo\nthree\n", "edit line1");
    write_commit("far", "main", "one\ntwo\nTHREE\n", "edit line3");
    write_commit("clash", "main", "1!!!\ntwo\nthree\n", "edit line1 differently");
    git(&repo, &home, &["checkout", "-q", "main"]);

    (repo, home)
}

/// Assert zvcs and git agree byte-for-byte on stdout, stderr, and exit code.
fn assert_parity(repo: &Path, home: &Path, args: &[&str], input: &str, what: &str) {
    let z = merge_tree(BIN, repo, home, args, input);
    let g = merge_tree("git", repo, home, args, input);
    assert_eq!(z.status.code(), g.status.code(), "{what}: exit code");
    assert_eq!(
        String::from_utf8_lossy(&z.stdout),
        String::from_utf8_lossy(&g.stdout),
        "{what}: stdout"
    );
    assert_eq!(
        String::from_utf8_lossy(&z.stderr),
        String::from_utf8_lossy(&g.stderr),
        "{what}: stderr"
    );
}

#[test]
fn stdin_clean_and_conflict_batch_matches_git() {
    let (repo, home) = fixture("mixed");
    // A clean merge then a conflicting merge in one batch: exercises both the
    // `1`/`0` clean flag and the conflict info + message block per record.
    assert_parity(
        &repo,
        &home,
        &["--stdin"],
        "ours far\nours clash\n",
        "clean then conflict",
    );
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn stdin_z_and_explicit_base_matches_git() {
    let (repo, home) = fixture("zbase");
    // `-z` switches every record separator to NUL; the `<base> -- <b1> <b2>`
    // line form supplies an explicit merge base.
    assert_parity(
        &repo,
        &home,
        &["--stdin", "-z"],
        "main -- ours far\nours clash\n",
        "-z with explicit base",
    );
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn stdin_malformed_line_dies_128_like_git() {
    let (repo, home) = fixture("malformed");
    // A single-token line has `split.nr < 2`, git's `die("malformed input
    // line: '%s'.")` — exit 128, aborting the batch.
    assert_parity(&repo, &home, &["--stdin"], "ours\n", "malformed single token");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
