//! Two `cmd_rm()` steps zvcs skipped.
//!
//! The entry loop keeps an entry out of the removal list when
//!
//! ```c
//! if (!include_sparse &&
//!     (ce_skip_worktree(ce) ||
//!      !path_in_sparse_checkout(ce->name, the_repository->index)))
//!         continue;
//! ```
//!
//! (builtin/rm.c:316-319), and `find_pathspecs_matching_skip_worktree()` tallies the same set
//! (pathspec.c:84). zvcs asked only for the skip-worktree bit, so a modified file that
//! `sparse-checkout set` left behind without the bit was removed where stock prints the
//! sparse-path report and exits 1.
//!
//! `if (!index_only) submodules_absorb_gitdir_if_needed();` (builtin/rm.c:368-369) moves a
//! submodule's `.git` directory into the superproject's `modules/<name>` before anything is
//! checked or removed, `-f` and `-n` included, and dies through `submodule_uses_gitfile()`'s
//! `read_gitfile()` on a `.git` file that leads nowhere (submodule.c:1962-1994). zvcs had
//! neither: `git rm -f sm` deleted the submodule's whole repository with its work tree.

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
        .env("GIT_EDITOR", "true")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    (
        out.status.code().expect("no signal"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn write(dir: &Path, rel: &str, body: &str) {
    let full = dir.join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, body).unwrap();
}

/// `a`, `dir/c` and `sub/e` committed on `main`.
fn base(stock: &str, root: &Path) -> PathBuf {
    let dir = root.join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    let ok = |args: &[&str]| assert_eq!(git(stock, &dir, args).0, 0, "{args:?}");
    ok(&["init", "-q", "-b", "main"]);
    for path in ["a", "dir/c", "sub/e"] {
        write(&dir, path, &format!("{path}\n"));
    }
    ok(&["add", "."]);
    ok(&["-c", "maintenance.auto=false", "commit", "-q", "-m", "base"]);
    dir
}

/// `base` with a non-cone sparse checkout of `/dir/` that leaves the modified `sub/e`
/// behind, without its skip-worktree bit.
fn left_behind(stock: &str, root: &Path) -> PathBuf {
    let dir = base(stock, root);
    write(&dir, "sub/e", "sub/e\nmodified\n");
    let set = git(stock, &dir, &["sparse-checkout", "set", "--no-cone", "/dir/"]);
    assert!(set.2.contains("left despite sparse patterns"), "{set:?}");
    dir
}

/// A superproject with `sm` registered by `submodule add`, its repository then put back
/// into the work tree as a `.git` directory.
fn embedded(stock: &str, root: &Path) -> PathBuf {
    let up = root.join("up");
    std::fs::create_dir_all(&up).unwrap();
    let ok = |dir: &Path, args: &[&str]| assert_eq!(git(stock, dir, args).0, 0, "{args:?}");
    ok(&up, &["init", "-q", "-b", "main"]);
    write(&up, "x", "x\n");
    ok(&up, &["add", "x"]);
    ok(&up, &["-c", "maintenance.auto=false", "commit", "-q", "-m", "x"]);
    let dir = base(stock, root);
    ok(&dir, &["-c", "protocol.file.allow=always", "submodule", "add", "-q", "../up", "sm"]);
    ok(&dir, &["-c", "maintenance.auto=false", "commit", "-q", "-m", "sm"]);
    std::fs::remove_file(dir.join("sm/.git")).unwrap();
    std::fs::rename(dir.join(".git/modules/sm"), dir.join("sm/.git")).unwrap();
    // The moved repository's `core.worktree` still names it from `.git/modules/sm`, which
    // a `config` run inside it would chdir to and die on: edited out of the file instead.
    let config = dir.join("sm/.git/config");
    let text = std::fs::read_to_string(&config).unwrap();
    let kept: String = text
        .lines()
        .filter(|l| !l.trim_start().starts_with("worktree ="))
        .map(|l| format!("{l}\n"))
        .collect();
    assert_ne!(kept, text);
    std::fs::write(&config, kept).unwrap();
    dir
}

/// `embedded`, with `sm/.git` a gitfile that leads nowhere.
fn broken_gitfile(stock: &str, root: &Path) -> PathBuf {
    let dir = embedded(stock, root);
    std::fs::remove_dir_all(dir.join("sm/.git")).unwrap();
    write(&dir, "sm/.git", "gitdir: nowhere\n");
    dir
}

/// Everything under `dir` but `.git`'s own internals, with `.git/modules` named.
fn tree(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(d) = pending.pop() {
        for entry in std::fs::read_dir(&d).unwrap().flatten() {
            let path = entry.path();
            let rel = path.strip_prefix(dir).unwrap().to_string_lossy().into_owned();
            if rel == ".git" {
                continue;
            }
            found.push(rel);
            if entry.file_type().unwrap().is_dir() {
                pending.push(path);
            }
        }
    }
    found.sort();
    found.push(format!("modules/sm/HEAD: {}", dir.join(".git/modules/sm/HEAD").exists()));
    found.push(format!("sm/.git: {:?}", std::fs::read_to_string(dir.join("sm/.git")).ok()));
    found
}

fn same(label: &str, fixture: fn(&str, &Path) -> PathBuf, args: &[&str]) {
    let Some(stock) = stock_git() else { return };
    let mut seen = Vec::new();
    for (bin, side) in [(stock, "stock"), (ZVCS, "zvcs")] {
        let root = std::env::temp_dir().join(format!("zvcs-rm-absorb-{label}-{side}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let dir = fixture(stock, &root);
        let run = git(bin, &dir, args);
        let shown = root.to_string_lossy().into_owned();
        seen.push((
            run.0,
            run.1.replace(&shown, "<root>"),
            run.2.replace(&shown, "<root>"),
            git(stock, &dir, &["ls-files", "-s"]).1,
            git(stock, &dir, &["status", "--porcelain", "--ignore-submodules=none"]).1,
            tree(&dir),
        ));
        let _ = std::fs::remove_dir_all(&root);
    }
    assert_eq!(seen[1], seen[0], "git {args:?}: left is zvcs, right is stock");
}

#[test]
fn an_entry_the_patterns_exclude_is_out_of_scope_without_its_bit() {
    same("left-r", left_behind, &["rm", "-r", "sub"]);
    same("left-file", left_behind, &["rm", "-f", "sub/e"]);
    same("left-cached", left_behind, &["rm", "--cached", "sub/e"]);
    same("left-dry", left_behind, &["rm", "-n", "-r", "."]);
    same("left-mixed", left_behind, &["rm", "-r", "dir", "sub"]);
    same("left-sparse", left_behind, &["rm", "-f", "--sparse", "sub/e"]);
    same("left-ignore", left_behind, &["rm", "--ignore-unmatch", "-r", "sub"]);
}

#[test]
fn a_submodule_git_directory_is_absorbed_before_removal() {
    same("absorb-force", embedded, &["rm", "-f", "sm"]);
    same("absorb-plain", embedded, &["rm", "sm"]);
    same("absorb-dry", embedded, &["rm", "-n", "sm"]);
    same("absorb-quiet", embedded, &["rm", "-q", "-f", "sm"]);
    same("absorb-cached", embedded, &["rm", "--cached", "sm"]);
}

#[test]
fn a_gitfile_leading_nowhere_dies_even_when_forced() {
    same("broken-force", broken_gitfile, &["rm", "-f", "sm"]);
    same("broken-plain", broken_gitfile, &["rm", "sm"]);
    same("broken-dry", broken_gitfile, &["rm", "-n", "-f", "sm"]);
}
