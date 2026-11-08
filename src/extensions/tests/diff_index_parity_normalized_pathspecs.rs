//! `git diff-index` with literal pathspecs that need normalising, compared against
//! stock git 2.56.0 on identical fixtures.
//!
//! `prefix_path_gently()` joins each element to the cwd prefix and normalises it
//! (setup.c:117-147), so from `sub/` the elements `.`, `./c`, `..` and `../a` are
//! `sub`, `sub/c`, the whole tree and `a`; an element that normalises to nothing
//! matches everything (`match_pathspec_item()`, dir.c:441-442). zvcs glued the raw
//! element to the prefix, so each of them matched no path at all, which also left
//! `git stash push -p` from a subdirectory offering nothing outside it.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(bin: &str, dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", "/nonexistent")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn dot_and_dotdot_elements_are_normalised_against_the_prefix() {
    let Some(stock) = stock_git() else { return };
    let root = std::env::temp_dir().join(format!("zvcs-diff-index-norm-{}", std::process::id()));
    let _guard = Dir(root.clone());
    std::fs::create_dir_all(root.join("sub/deep")).unwrap();
    let g = |args: &[&str]| {
        let mut full = vec!["-c", "maintenance.auto=false"];
        full.extend_from_slice(args);
        let out = run(stock, &root, &full);
        assert_eq!(out.0, 0, "setup {args:?}: {out:?}");
    };
    g(&["init", "-q", "-b", "main", "."]);
    for (path, body) in [("a", "a\n"), ("b", "b\n"), ("sub/c", "c\n"), ("sub/deep/d", "d\n")] {
        std::fs::write(root.join(path), body).unwrap();
    }
    g(&["add", "."]);
    g(&["commit", "-q", "-m", "init"]);
    for (path, body) in [("a", "A\n"), ("b", "B\n"), ("sub/c", "C\n"), ("sub/deep/d", "D\n")] {
        std::fs::write(root.join(path), body).unwrap();
    }
    g(&["add", "b"]);

    for (cwd, pathspec) in [
        ("", &["."][..]),
        ("", &["./sub/../a"]),
        ("", &["sub/./deep/"]),
        ("sub", &["."]),
        ("sub", &[".."]),
        ("sub", &["./c"]),
        ("sub", &["../a", "c"]),
        ("sub", &["deep/.."]),
        ("sub/deep", &["../../b", "."]),
    ] {
        for cached in [false, true] {
            let mut args = vec!["diff-index", "--name-status"];
            if cached {
                args.push("--cached");
            }
            args.extend(["HEAD", "--"]);
            args.extend_from_slice(pathspec);
            let dir = root.join(cwd);
            assert_eq!(run(stock, &dir, &args), run(BIN, &dir, &args), "git {args:?} from {cwd:?}");
        }
    }
}
