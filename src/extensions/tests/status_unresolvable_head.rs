//! `git status` over a `HEAD` that does not resolve at all.
//!
//! `wt_status_prepare()` reads `s->branch = refs_resolve_refdup(…, "HEAD", 0,
//! NULL, NULL)` (wt-status.c:153), which is NULL — not fatal — when `HEAD` is a
//! symref to a malformed name. That is exactly what `init --separate-git-dir`
//! leaves behind when it moves a reftable repository: the new `config` loses
//! `extensions.refStorage`, so the files backend reads the reftable stub `HEAD`
//! (`ref: refs/heads/.invalid`). Each format then has its own NULL-branch arm:
//!
//! * long: `if (s->branch)` guards the branch line (wt-status.c:1998);
//! * short `-b`: `## ` and then `return` with no terminator (wt-status.c:2207-2210);
//! * porcelain v2 `--branch`: `# branch.head (unknown)` (wt-status.c:2347-2348).
//!
//! The port used to die on the unparseable `HEAD` instead. Expectations were
//! measured from stock git 2.56.0.

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", home.join("gitconfig"))
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("spawn git")
}

fn ok(dir: &Path, home: &Path, args: &[&str]) -> Vec<u8> {
    let out = run(dir, home, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// A repository with `f` and `g` staged and a `HEAD` the files backend cannot
/// read.
fn fixture(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let root = std::env::temp_dir()
        .join(format!("zvcs-unresolvable-head-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(home.join("gitconfig"), "").unwrap();
    ok(&repo, &home, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("f"), "a\n").unwrap();
    std::fs::write(repo.join("g"), "b\n").unwrap();
    ok(&repo, &home, &["add", "f", "g"]);
    std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/.invalid\n").unwrap();
    (repo, home)
}

#[test]
fn porcelain_treats_an_unresolvable_head_as_initial() {
    let (repo, home) = fixture("t1");
    assert_eq!(ok(&repo, &home, &["status", "--porcelain"]), b"A  f\nA  g\n");
}

#[test]
fn short_branch_header_stops_after_its_prefix() {
    let (repo, home) = fixture("t2");
    assert_eq!(ok(&repo, &home, &["status", "-sb"]), b"## A  f\nA  g\n");
    assert_eq!(ok(&repo, &home, &["status", "-sbz"]), b"## A  f\0A  g\0");
}

#[test]
fn porcelain_v2_names_the_branch_unknown() {
    let (repo, home) = fixture("t3");
    let out = ok(&repo, &home, &["status", "--porcelain=v2", "--branch"]);
    let expected = "# branch.oid (initial)\n\
                    # branch.head (unknown)\n\
                    1 A. N... 000000 100644 100644 0000000000000000000000000000000000000000 78981922613b2afb6025042ff6bd878ac1994e85 f\n\
                    1 A. N... 000000 100644 100644 0000000000000000000000000000000000000000 61780798228d17af2d34fce4cfbdf35556832472 g\n";
    assert_eq!(String::from_utf8(out).unwrap(), expected);
}

#[test]
fn long_format_omits_the_branch_line() {
    let (repo, home) = fixture("t4");
    let out = ok(&repo, &home, &["status"]);
    let expected = "\n\
                    No commits yet\n\
                    \n\
                    Changes to be committed:\n  \
                    (use \"git rm --cached <file>...\" to unstage)\n\
                    \tnew file:   f\n\
                    \tnew file:   g\n\
                    \n";
    assert_eq!(String::from_utf8(out).unwrap(), expected);
}
