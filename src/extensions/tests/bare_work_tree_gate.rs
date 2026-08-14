//! Who refuses in a bare repository, and who does not — pinned against stock git
//! 2.55.0.
//!
//! * **The command-table gate.** `run_builtin()` (git.c:499-500) runs
//!   `setup_work_tree()` for every command flagged `NEED_WORK_TREE` before the
//!   builtin is entered, so those refuse whatever they were asked to do. A lone
//!   `-h` skips it (git.c:474-477 demotes the setup for that case).
//!
//! * **The per-option gates.** A command not in that table may still need a work
//!   tree for some of its options and asks for one itself. `ls-files` does it for
//!   `-m`, `-o`, `-d`, `-i` and `-k` (builtin/ls-files.c:707-708, 720-721) and for
//!   nothing else, so `-c`, `-s`, `-u`, `-t`, `--directory`, `--exclude-standard`
//!   and `--resolve-undo` all list a bare repository's index happily. `grep` does
//!   it only for the default search (builtin/grep.c:1416-1418).
//!
//! * **`GIT_WORK_TREE` satisfies all of them.** `setup_explicit_git_dir()`
//!   (setup.c:1142) installs the environment's work tree before `core.bare` is
//!   read, so a bare repository handed one is a repository with a work tree.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

const NEED_WORK_TREE: &str = "fatal: this operation must be run in a work tree\n";

fn git_env(dir: &Path, home: &Path, args: &[&str], work_tree: Option<&Path>) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.args(["-c", "user.email=t@e.x", "-c", "user.name=t"])
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE");
    if let Some(wt) = work_tree {
        cmd.env("GIT_WORK_TREE", wt);
    }
    cmd.output().expect("run binary")
}

fn git(dir: &Path, home: &Path, args: &[&str]) -> Output {
    git_env(dir, home, args, None)
}

fn ok(dir: &Path, home: &Path, args: &[&str]) -> Output {
    let o = git(dir, home, args);
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    o
}

/// A worktree repository with one commit, and a bare clone of it. The clone has
/// no index, so everything the work tree holds is untracked from over there.
fn bare_clone(tag: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-baregate-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::fs::create_dir_all(root.join("work/sub")).unwrap();
    let root = root.canonicalize().unwrap();
    let (home, work) = (root.join("home"), root.join("work"));

    ok(&work, &home, &["init", "-q", "-b", "main"]);
    std::fs::write(work.join("a.txt"), "a\n").unwrap();
    std::fs::write(work.join("sub/b.txt"), "b\n").unwrap();
    ok(&work, &home, &["add", "a.txt", "sub/b.txt"]);
    ok(&work, &home, &["commit", "-q", "-m", "one"]);

    let bare = root.join("bare.git");
    ok(&root, &home, &["clone", "-q", "--bare", work.to_str().unwrap(), bare.to_str().unwrap()]);
    (root, home, work, bare)
}

fn head(dir: &Path, home: &Path) -> String {
    String::from_utf8_lossy(&ok(dir, home, &["rev-parse", "HEAD"]).stdout).trim().to_string()
}

#[test]
fn ls_files_worktree_selectors_refuse_in_a_bare_repository() {
    let (root, home, _work, bare) = bare_clone("lsfiles");

    // The five selectors of `require_work_tree`, alone and in the combinations
    // that reach it through another option.
    for args in [
        &["ls-files", "-o"][..],
        &["ls-files", "-m"][..],
        &["ls-files", "-d"][..],
        &["ls-files", "-k"][..],
        &["ls-files", "-i"][..],
        &["ls-files", "-i", "-o"][..],
        &["ls-files", "-o", "--directory"][..],
        &["ls-files", "--exclude-standard", "-o"][..],
        // The gate precedes the `-i` guards, so this is not "-i must be used with
        // either -o or -c" and not "--ignored needs some exclude pattern".
        &["ls-files", "-i", "--directory"][..],
    ] {
        let o = git(&bare, &home, args);
        assert_eq!(o.status.code(), Some(128), "git {args:?} exit: {o:?}");
        assert_eq!(String::from_utf8_lossy(&o.stderr), NEED_WORK_TREE, "git {args:?} stderr");
    }

    // Everything else reads the index alone and works: the clone has none, so each
    // prints nothing and exits 0.
    for args in [
        &["ls-files"][..],
        &["ls-files", "-c"][..],
        &["ls-files", "-s"][..],
        &["ls-files", "-u"][..],
        &["ls-files", "-t"][..],
        &["ls-files", "--directory"][..],
        &["ls-files", "--exclude-standard"][..],
        &["ls-files", "--resolve-undo"][..],
        &["ls-files", "--format=%(path)"][..],
    ] {
        let o = git(&bare, &home, args);
        assert_eq!(o.status.code(), Some(0), "git {args:?} exit: {o:?}");
        assert_eq!(String::from_utf8_lossy(&o.stdout), "", "git {args:?} stdout");
        assert_eq!(String::from_utf8_lossy(&o.stderr), "", "git {args:?} stderr");
    }

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_work_tree_from_the_environment_satisfies_the_gate() {
    let (root, home, work, bare) = bare_clone("envwt");

    // With a work tree supplied, the walk is legal and lists it. Nothing in the
    // clone's (absent) index is tracked, so every file is an "other".
    let o = git_env(&bare, &home, &["ls-files", "-o"], Some(&work));
    assert_eq!(o.status.code(), Some(0), "{o:?}");
    assert_eq!(String::from_utf8_lossy(&o.stdout), "a.txt\nsub/b.txt\n");

    // The index-only modes are unaffected by it.
    let o = git_env(&bare, &home, &["ls-files"], Some(&work));
    assert_eq!(o.status.code(), Some(0), "{o:?}");
    assert_eq!(String::from_utf8_lossy(&o.stdout), "");

    // The repository now has a work tree, so the commands gated on one run.
    let o = git_env(&bare, &home, &["status", "--porcelain"], Some(&work));
    assert_eq!(o.status.code(), Some(0), "{o:?}");
    assert!(
        String::from_utf8_lossy(&o.stdout).contains("?? a.txt"),
        "status did not see the work tree: {o:?}"
    );

    // A work tree that is not there is what `setup_work_tree()`'s failed `chdir()`
    // reports, in the same words as no work tree at all.
    let missing = root.join("gone");
    let o = git_env(&bare, &home, &["ls-files", "-o"], Some(&missing));
    assert_eq!(o.status.code(), Some(128), "{o:?}");
    assert_eq!(String::from_utf8_lossy(&o.stderr), NEED_WORK_TREE);

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn need_work_tree_commands_refuse_in_a_bare_repository() {
    let (root, home, _work, bare) = bare_clone("table");
    let before = head(&bare, &home);

    // git.c's `NEED_WORK_TREE` set, minus the two that would read stdin or talk to
    // a remote (`am`, `pull`). Each refuses before it can say anything of its own —
    // no usage error, no "Already up to date.", no commit.
    for args in [
        &["add", "."][..],
        &["check-ignore", "a.txt"][..],
        &["checkout", "main"][..],
        &["checkout-index", "-a"][..],
        &["cherry-pick", "HEAD"][..],
        &["clean", "-n"][..],
        &["commit", "-m", "x"][..],
        &["diff-files"][..],
        &["merge", "main"][..],
        &["merge-recursive"][..],
        &["mv", "a.txt", "c.txt"][..],
        &["rebase", "main"][..],
        &["restore", "."][..],
        &["revert", "HEAD"][..],
        &["stage", "."][..],
        &["stash"][..],
        &["stash", "list"][..],
        &["status"][..],
        &["switch", "main"][..],
    ] {
        let o = git(&bare, &home, args);
        assert_eq!(o.status.code(), Some(128), "git {args:?} exit: {o:?}");
        assert_eq!(String::from_utf8_lossy(&o.stderr), NEED_WORK_TREE, "git {args:?} stderr");
        assert_eq!(String::from_utf8_lossy(&o.stdout), "", "git {args:?} stdout");
        assert_eq!(head(&bare, &home), before, "git {args:?} moved HEAD");
    }

    // `git <cmd> -h` is exempt: git demotes the setup for it, so the answer is the
    // usage error (129), never the work-tree refusal.
    for args in [&["status", "-h"][..], &["commit", "-h"][..]] {
        let o = git(&bare, &home, args);
        assert_eq!(o.status.code(), Some(129), "git {args:?} exit: {o:?}");
        assert!(
            !String::from_utf8_lossy(&o.stderr).contains("must be run in a work tree"),
            "git {args:?} refused a help request: {o:?}"
        );
    }

    // Commands with no work-tree flag keep working there.
    for args in [&["log", "--oneline", "-1"][..], &["rev-parse", "HEAD"][..], &["cat-file", "-t", "HEAD"][..]] {
        let o = git(&bare, &home, args);
        assert_eq!(o.status.code(), Some(0), "git {args:?} exit: {o:?}");
    }

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn grep_needs_a_work_tree_only_for_its_default_search() {
    let (root, home, work, bare) = bare_clone("grep");

    // No `<tree>`, no `--cached`: the search reads worktree files.
    for args in [&["grep", "a"][..], &["grep", "-e", "a", "--and", "-e", "b"][..]] {
        let o = git(&bare, &home, args);
        assert_eq!(o.status.code(), Some(128), "git {args:?} exit: {o:?}");
        assert_eq!(String::from_utf8_lossy(&o.stderr), NEED_WORK_TREE, "git {args:?} stderr");
    }

    // The same search over a supplied work tree is not gated, and finds the file.
    let o = git_env(&bare, &home, &["grep", "a"], Some(&work));
    assert_eq!(o.status.code(), Some(1), "nothing is tracked over there: {o:?}");
    assert_eq!(String::from_utf8_lossy(&o.stderr), "");

    // A `<tree>` search reads blobs and needs no work tree. `verify_non_filename()`
    // (setup.c:299-302) stands down outside one, so `HEAD` is a revision here even
    // though a file by that name is sitting in the cwd.
    assert!(bare.join("HEAD").is_file(), "the ambiguity this guards against is real");
    let o = git(&bare, &home, &["grep", "a", "HEAD"]);
    assert_eq!(o.status.code(), Some(0), "{o:?}");
    assert_eq!(String::from_utf8_lossy(&o.stdout), "HEAD:a.txt:a\n");
    assert_eq!(String::from_utf8_lossy(&o.stderr), "");

    // `--no-index` and `--untracked` reach `grep_directory()` (builtin/grep.c:1411),
    // which walks the current directory rather than a work tree — here, the git
    // directory itself, with paths relative to it. The two differ by
    // `setup_standard_excludes()`: `--untracked` resolves `--exclude-standard` on,
    // so `info/exclude` hides a file that `--no-index` still finds.
    std::fs::write(bare.join("info/exclude"), "hidden.txt\n").unwrap();
    std::fs::write(bare.join("hidden.txt"), "needle\n").unwrap();
    std::fs::write(bare.join("found.txt"), "needle\n").unwrap();

    let o = git(&bare, &home, &["grep", "--untracked", "-l", "needle"]);
    assert_eq!(o.status.code(), Some(0), "{o:?}");
    assert_eq!(String::from_utf8_lossy(&o.stdout), "found.txt\n");

    let o = git(&bare, &home, &["grep", "--no-index", "-l", "needle"]);
    assert_eq!(o.status.code(), Some(0), "{o:?}");
    assert_eq!(String::from_utf8_lossy(&o.stdout), "found.txt\nhidden.txt\n");

    // A pathspec still limits that walk.
    let o = git(&bare, &home, &["grep", "--no-index", "-l", "needle", "--", "found.txt"]);
    assert_eq!(o.status.code(), Some(0), "{o:?}");
    assert_eq!(String::from_utf8_lossy(&o.stdout), "found.txt\n");

    let _ = std::fs::remove_dir_all(root);
}
