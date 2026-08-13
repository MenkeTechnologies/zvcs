//! Repository setup from directories that are not the top of a work tree.
//!
//! Every case here is a port of `setup_git_directory_gently_1()` in git's `setup.c`, which probes
//! `<dir>/.git` and then `<dir>` itself on the way up. The second probe is the one that is easy to
//! miss: a hit on `<dir>` itself is `GIT_DIR_BARE`, so the directory *becomes* `$GIT_DIR` and
//! `setup_bare_git_dir()` turns the implicit work tree off — which is what lets `git log` run from
//! inside a `.git` directory and what makes `git status` there fail with "this operation must be
//! run in a work tree" rather than reporting on the repository's own administrative files.
//!
//! The parity corpus cannot express these: every corpus case runs with the working directory at
//! the top of the work tree, so the whole `GIT_DIR_BARE` half of discovery is invisible to it.
//! Expectations below were taken from stock git 2.55.0 run in the same layouts.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_CEILING_DIRECTORIES")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .expect("run binary")
}

fn git(dir: &Path, home: &Path, args: &[&str]) {
    let o = run(dir, home, args);
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
}

/// Stdout with the trailing newline removed, asserting the command succeeded.
fn stdout(dir: &Path, home: &Path, args: &[&str]) -> String {
    let o = run(dir, home, args);
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim_end_matches('\n').to_owned()
}

/// A repo with one commit, a linked worktree, and a bare clone beside them.
fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-disc-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    // macOS reaches the temp directory through a symlink and both binaries record the resolved
    // path, so every expectation has to be built from the resolved root.
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    let repo = root.join("main");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &home, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("f"), "x\n").unwrap();
    git(&repo, &home, &["add", "f"]);
    git(&repo, &home, &["commit", "-q", "-m", "c1"]);
    (root, repo, home)
}

/// The reported bug: from inside `.git`, reads have to work against that directory rather than
/// look for a `.git/.git` that does not exist.
#[test]
fn reads_work_from_within_the_git_dir() {
    let (_root, repo, home) = fixture("in-gitdir");
    let git_dir = repo.join(".git");
    let head = stdout(&repo, &home, &["rev-parse", "HEAD"]);

    assert_eq!(
        stdout(&git_dir, &home, &["rev-parse", "HEAD"]),
        head,
        "the git dir resolves the same commit as the work tree does"
    );
    assert_eq!(stdout(&git_dir, &home, &["symbolic-ref", "HEAD"]), "refs/heads/main");
    assert_eq!(
        stdout(&git_dir, &home, &["log", "--oneline"]).lines().count(),
        1,
        "`git log` from inside `.git` is what the gitoxide shallow fixtures rely on"
    );
}

/// `setup_bare_git_dir()` sets `$GIT_DIR` to `.` when the cwd *is* the git directory, and to the
/// absolute path of the git directory the walk stopped at when it is above the cwd.
#[test]
fn git_dir_is_reported_the_way_setup_leaves_it() {
    let (_root, repo, home) = fixture("gitdir-value");
    let git_dir = repo.join(".git");

    assert_eq!(stdout(&repo, &home, &["rev-parse", "--git-dir"]), ".git");
    assert_eq!(stdout(&git_dir, &home, &["rev-parse", "--git-dir"]), ".");
    for below in ["refs/heads", "objects"] {
        assert_eq!(
            stdout(&git_dir.join(below), &home, &["rev-parse", "--git-dir"]),
            git_dir.to_string_lossy(),
            "from `.git/{below}` the walk stops at `.git` and reports it absolutely"
        );
    }
}

/// `GIT_DIR_BARE` means no work tree, so the predicates flip and the commands that need one die
/// the way git dies rather than failing somewhere deeper.
#[test]
fn the_git_dir_has_no_work_tree() {
    let (_root, repo, home) = fixture("no-worktree");
    let git_dir = repo.join(".git");

    for dir in [git_dir.clone(), git_dir.join("refs")] {
        assert_eq!(stdout(&dir, &home, &["rev-parse", "--is-inside-work-tree"]), "false");
        assert_eq!(stdout(&dir, &home, &["rev-parse", "--is-inside-git-dir"]), "true");

        for args in [vec!["status", "--porcelain"], vec!["rev-parse", "--show-toplevel"]] {
            let o = run(&dir, &home, &args);
            assert_eq!(o.status.code(), Some(128), "git {args:?} in {dir:?} exits like git's die()");
            assert_eq!(
                String::from_utf8_lossy(&o.stderr),
                "fatal: this operation must be run in a work tree\n",
                "git {args:?} in {dir:?}"
            );
        }
    }
}

/// A bare repository's subdirectory reaches the git directory through the same second probe. This
/// used to abort the process: the walk had turned the cursor absolute, and the path-shortening
/// helper it then called asserts that the path it is given ends in `.git`.
#[test]
fn discovery_from_a_bare_repository_subdirectory() {
    let (root, repo, home) = fixture("bare-subdir");
    let bare = root.join("bare.git");
    git(&repo, &home, &["clone", "-q", "--bare", ".", bare.to_str().unwrap()]);

    for dir in [bare.clone(), bare.join("refs"), bare.join("objects")] {
        let o = run(&dir, &home, &["rev-parse", "--git-dir"]);
        assert!(
            o.status.code() != Some(101),
            "discovery must not panic in {dir:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        let expected = if dir == bare { ".".to_owned() } else { bare.to_string_lossy().into_owned() };
        assert_eq!(String::from_utf8_lossy(&o.stdout).trim_end(), expected);
        assert_eq!(stdout(&dir, &home, &["rev-parse", "--is-bare-repository"]), "true");
    }
}

/// A linked worktree has three interesting directories, and git reports a different git dir in
/// each: the checkout resolves its `.git` file, the private git dir is adopted as `.`, and the
/// common dir is shared by both.
#[test]
fn linked_worktree_checkout_and_private_git_dir() {
    let (root, repo, home) = fixture("linked-wt");
    let checkout = root.join("wt");
    git(&repo, &home, &["worktree", "add", "-q", checkout.to_str().unwrap(), "-b", "wtb"]);
    let private = repo.join(".git/worktrees/wt");
    let common = repo.join(".git");

    // At the checkout, `.git` is a *file*, so setup resolves it and stores the absolute private
    // git dir instead of leaving `$GIT_DIR` at its `.git` default.
    assert_eq!(
        stdout(&checkout, &home, &["rev-parse", "--git-dir"]),
        private.to_string_lossy()
    );
    assert_eq!(
        stdout(&checkout, &home, &["rev-parse", "--git-common-dir"]),
        common.to_string_lossy()
    );
    assert_eq!(stdout(&checkout, &home, &["rev-parse", "--is-inside-work-tree"]), "true");

    // Standing in the private git dir is `GIT_DIR_BARE` again: git does not follow the `gitdir`
    // back-link to re-attach the checkout.
    assert_eq!(stdout(&private, &home, &["rev-parse", "--git-dir"]), ".");
    assert_eq!(
        stdout(&private, &home, &["rev-parse", "--git-common-dir"]),
        common.to_string_lossy()
    );
    assert_eq!(stdout(&private, &home, &["rev-parse", "--is-inside-work-tree"]), "false");
    assert_eq!(stdout(&private, &home, &["symbolic-ref", "HEAD"]), "refs/heads/wtb");
}

/// `setup_git_directory_gently_1()` returns `GIT_DIR_EXPLICIT` before it walks anywhere, so
/// `$GIT_DIR` is used verbatim — including `.` from inside a `.git` directory, where the work tree
/// then becomes the cwd via `set_git_work_tree(repo, ".")`.
#[test]
fn explicit_git_dir_is_used_verbatim() {
    let (root, repo, home) = fixture("explicit");
    let git_dir = repo.join(".git");
    let head = stdout(&repo, &home, &["rev-parse", "HEAD"]);

    let o = Command::new(BIN)
        .args(["rev-parse", "--git-dir", "HEAD"])
        .current_dir(&git_dir)
        .env("HOME", &home)
        .env("ZVCS_HOME", &home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_DIR", ".")
        .output()
        .expect("run binary");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(String::from_utf8_lossy(&o.stdout), format!(".\n{head}\n"));

    // From a directory outside any repository, `$GIT_DIR` alone still finds it.
    let outside = root.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let o = Command::new(BIN)
        .args(["log", "--oneline"])
        .current_dir(&outside)
        .env("HOME", &home)
        .env("ZVCS_HOME", &home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_DIR", &git_dir)
        .output()
        .expect("run binary");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(String::from_utf8_lossy(&o.stdout).lines().count(), 1);
}

/// A `GIT_CEILING_DIRECTORIES` that names no ancestor of the search directory is not an error in
/// git: `setup_git_directory_gently_1()` folds it away and searches as if it were unset. One that
/// *does* contain an ancestor stops the walk there.
#[test]
fn ceiling_directories_bound_the_walk() {
    let (root, repo, home) = fixture("ceiling");
    let deep = repo.join("sub/deep");
    std::fs::create_dir_all(&deep).unwrap();

    let with_ceiling = |dir: &Path, ceiling: &Path| {
        Command::new(BIN)
            .args(["rev-parse", "--git-dir"])
            .current_dir(dir)
            .env("HOME", &home)
            .env("ZVCS_HOME", &home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CEILING_DIRECTORIES", ceiling)
            .output()
            .expect("run binary")
    };

    let o = with_ceiling(&deep, &root.join("nowhere"));
    assert!(o.status.success(), "a non-matching ceiling is ignored, not an error");
    assert_eq!(
        String::from_utf8_lossy(&o.stdout).trim_end(),
        repo.join(".git").to_string_lossy()
    );

    let o = with_ceiling(&deep, &repo.join("sub"));
    assert_eq!(o.status.code(), Some(128), "a matching ceiling stops the walk below the repo");
}
