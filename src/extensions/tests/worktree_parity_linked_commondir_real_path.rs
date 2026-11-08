//! Discovery from inside a linked worktree whose `commondir` names a path
//! `strbuf_add_real_path()` cannot resolve.
//!
//! `is_git_directory()` takes the common directory from `get_common_dir_noenv()`
//! (setup.c:312-350), which resolves `<gitdir>/<commondir>` with `strbuf_realpath(..., 1)`
//! (abspath.c:78-200, :318-327): `..` is applied to what was resolved so far, symlinks
//! are followed, and any component but the last that `lstat()` cannot find dies
//! `Invalid path '<resolved so far>': <strerror>`. zvcs canonicalized the whole path,
//! fell back to it unresolved, and reported the gitfile as not pointing to a repository.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Twin {
    root: PathBuf,
}

impl Drop for Twin {
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
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

/// `<root>/r` with one commit and a linked worktree `<root>/wt` whose `commondir` file
/// holds `commondir`; `.git/dangle` is a dangling symlink, `.git/self` points at `.`
/// and `.git/loop1` and `.git/loop2` at each other.
fn prepare(stock: &str, root: &Path, commondir: &str) {
    let r = root.join("r");
    std::fs::create_dir_all(&r).unwrap();
    std::fs::write(r.join("top"), "1\n").unwrap();
    for args in [
        &["init", "-q", "-b", "main", "."][..],
        &["add", "."],
        &["-c", "maintenance.auto=false", "commit", "-q", "-m", "one"],
        &["worktree", "add", "-q", "../wt"],
    ] {
        assert_eq!(run(stock, &r, args).2, 0, "{args:?}");
    }
    #[cfg(unix)]
    for (link, target) in [("dangle", "../nowhere"), ("self", "."), ("loop1", "loop2"), ("loop2", "loop1")] {
        std::os::unix::fs::symlink(target, r.join(".git").join(link)).unwrap();
    }
    std::fs::write(r.join(".git/worktrees/wt/commondir"), commondir).unwrap();
}

fn compare(tag: &str, commondir: &str) {
    let Some(stock) = stock_git::stock_git() else {
        return;
    };
    let base = std::env::temp_dir().join(format!("zvcs-commondir-real-path-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let _twin = Twin { root: base.clone() };
    let mut outputs = Vec::new();
    for (side, bin) in [("s", stock), ("z", BIN)] {
        let root = base.join(side);
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        prepare(stock, &root, commondir);
        let cut = |s: String| s.replace(root.to_str().unwrap(), "ROOT");
        let mut out = Vec::new();
        for args in [
            &["status", "--porcelain"][..],
            &["rev-parse", "--git-common-dir"],
            &["log", "--oneline"],
            &["worktree", "list"],
        ] {
            let (o, e, c) = run(bin, &root.join("wt"), args);
            out.push((cut(o), cut(e), c));
        }
        outputs.push(out);
    }
    assert_eq!(outputs[1], outputs[0], "{tag}");
}

#[test]
fn a_missing_inner_component_dies_naming_the_path_resolved_so_far() {
    compare("missing-dotdot", "../../missing/../..\n");
    compare("missing-dir", "../../missing/x\n");
    compare("missing-sibling", "../../../nope/.git\n");
    compare("not-a-directory", "../../HEAD/x\n");
}

#[test]
fn a_missing_last_component_is_resolved_and_refused_as_before() {
    compare("missing-last", "../../missing\n");
    compare("a-file", "../../HEAD\n");
}

#[cfg(unix)]
#[test]
fn symlinks_are_followed_component_by_component() {
    compare("through-self", "../../self/../.git\n");
    compare("self-then-missing", "../../self/self/missing/..\n");
    compare("dangling", "../../dangle/x\n");
    compare("dangling-last", "../../dangle\n");
    compare("loop", "../../loop1/x\n");
}

#[test]
fn the_real_commondir_still_resolves() {
    compare("real", "../..\n");
}
