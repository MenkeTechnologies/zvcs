//! Which diagnostic comes first when a command line carries two things stock git
//! refuses, and what a refused `write-tree --prefix` leaves behind.
//!
//! * `%(refname:short)` asks `repo_settings_get_warn_ambiguous_refs()` for its
//!   `strict` argument (ref-filter.c `show_ref()`), and that call runs
//!   `prepare_repo_settings()` first — so a bad `core.maxTreeDepth` kills
//!   `tag --format=%(refname:short)` and `for-each-ref` with it, while the same
//!   listing with `%(refname)` runs.
//! * `fsck-objects` is `cmd_fsck` under another name: it reads `git_default_config`
//!   like `fsck` does.
//! * `pickaxe` is `cmd_blame` under another name: `blame.showRoot=always` dies in the
//!   blame config callback before any usage error.
//! * `cmd_gc()` parses its options and refuses an operand with `usage_with_options()`
//!   (129) before anything prepares the repository settings.
//! * `--max-parents=` / `--min-parents=` take `parse_count()` (`strtol_i`): a leading
//!   blank is fine and a negative bound means "no bound".
//! * `reset --pathspec-from-file` refuses `--patch` before reading the file, and a
//!   file that cannot be opened is `could not open '<f>' for reading: <errno text>`.
//! * `write-tree --prefix=<absent>/` dies without rewriting the index
//!   (`write_index_as_tree()` only writes when `write_index_as_tree_internal()`
//!   succeeded, cache-tree.c:818), so the repaired `TREE` extension is not stored.
//!
//! Expectations come from the stock git the other tests use as oracle.

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn run(bin: &str, dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .output()
        .unwrap_or_else(|e| panic!("{bin} {args:?}: {e}"));
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

/// A repository of two commits and two tags, built by `bin`.
fn fixture(bin: &str, tag: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!(
        "zvcs-error-path-order-{tag}-{}-{}",
        std::process::id(),
        if bin == BIN { "zvcs" } else { "stock" }
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("sub")).unwrap();
    run(bin, &root, &["init", "-q", "-b", "main", "."]);
    std::fs::write(root.join("a"), "a\n").unwrap();
    std::fs::write(root.join("sub/b"), "b\n").unwrap();
    run(bin, &root, &["add", "a", "sub/b"]);
    run(bin, &root, &["commit", "-q", "-m", "one"]);
    run(bin, &root, &["tag", "light"]);
    std::fs::write(root.join("a"), "a2\n").unwrap();
    run(bin, &root, &["commit", "-q", "-a", "-m", "two"]);
    run(bin, &root, &["tag", "-a", "-m", "annotated", "ann"]);
    Fixture { root }
}

/// Run `setup` then `args` in a fresh fixture on each binary and require the same
/// stdout, stderr and exit status.
fn same_answer(name: &str, setup: &[&[&str]], args: &[&str]) {
    let Some(stock) = stock_git() else { return };
    let mut answers = Vec::new();
    for bin in [stock, BIN] {
        let f = fixture(bin, name);
        for step in setup {
            run(bin, &f.root, step);
        }
        answers.push(run(bin, &f.root, args));
    }
    assert_eq!(answers[1], answers[0], "{args:?} after {setup:?}: zvcs differs from stock");
}

#[test]
fn short_refname_prepares_the_repository_settings() {
    let bad = [&["config", "core.maxTreeDepth", ""][..]];
    same_answer("short-tag", &bad, &["tag", "--format=%(refname:short)"]);
    same_answer("short-fer", &bad, &["for-each-ref", "--format=%(refname:short)"]);
    same_answer("short-sort", &bad, &["tag", "--sort=refname:short"]);
    // The full name never asks for `warn_ambiguous_refs`.
    same_answer("full-tag", &bad, &["tag", "--format=%(refname)"]);
}

#[test]
fn reset_pathspec_from_file_checks_in_git_order() {
    same_answer("reset-patch", &[], &["reset", "-p", "--pathspec-from-file=nofile"]);
    same_answer("reset-missing", &[], &["reset", "--pathspec-from-file=nofile"]);
    same_answer("reset-missing-numeric", &[], &["reset", "--pathspec-from-file=999999999", "--mixed"]);
    same_answer("reset-nul", &[], &["reset", "--pathspec-file-nul"]);
    same_answer("reset-with-operand", &[], &["reset", "--pathspec-from-file=nofile", "a"]);
}

#[test]
fn write_tree_with_an_absent_prefix_leaves_the_index_unrepaired() {
    let Some(stock) = stock_git() else { return };
    let mut states = Vec::new();
    for bin in [stock, BIN] {
        let f = fixture(bin, "wt-prefix");
        // Stage a path after the commit so the root of the cache-tree is invalid.
        std::fs::write(f.root.join("new"), "n\n").unwrap();
        run(bin, &f.root, &["add", "new"]);
        let before = std::fs::read(f.root.join(".git/index")).unwrap();
        let answer = run(bin, &f.root, &["write-tree", "--prefix=does-not-exist/"]);
        let after = std::fs::read(f.root.join(".git/index")).unwrap();
        states.push((answer, before == after));
    }
    assert_eq!(states[1], states[0], "zvcs differs from stock");
    assert_eq!(states[0].0 .2, 128);
    assert!(states[0].1, "stock rewrote the index");
}
