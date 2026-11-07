//! `stash apply --index` (and `merge`'s `restore_state()`, which runs it) from a
//! subdirectory.
//!
//! `do_apply_stash()` stages the stash's index through a `git apply --cached`
//! child (`apply_cached()`, builtin/stash.c:442-454). git's parent has already
//! moved to the top-level directory (setup.c:1957-1959), so the child starts
//! there. zvcs stays in the cwd, and it handed the child a `GIT_INDEX_FILE` built
//! from a relative `../.git`: the child, started in the top-level directory,
//! resolved it to a file that does not exist, read an empty index and failed with
//! `b/y: does not exist in index` and `conflicts in index. Try without --index.`
//! A `merge` refused over staged changes printed both lines after its refusal,
//! and `stash apply --index` from a subdirectory refused a stash it should apply.
//!
//! Every case runs stock git and zvcs on identical fixtures and compares exit
//! code, stdout, stderr and the resulting `status --porcelain`.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

type Outcome = (i32, String, String);

impl Fixture {
    /// `main` changed `a/x`, `side` changed `t`;
    /// built with `bin` so each binary's fixture is its own.
    fn new(bin: &str, tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-stash-idx-subdir-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::create_dir_all(root.join("b")).unwrap();
        let f = Fixture { root };
        f.git(bin, &["init", "-q", "-b", "main", "."]);
        f.write("a/x", "1\n");
        f.write("b/y", "1\n");
        f.write("t", "1\n");
        f.git(bin, &["add", "."]);
        f.git(bin, &["commit", "-q", "-m", "init"]);
        f.git(bin, &["checkout", "-q", "-b", "side"]);
        f.write("t", "2\n");
        f.git(bin, &["commit", "-q", "-am", "side"]);
        f.git(bin, &["checkout", "-q", "main"]);
        f.write("a/x", "3\n");
        f.git(bin, &["commit", "-q", "-am", "main"]);
        f
    }

    fn write(&self, rel: &str, body: &str) {
        std::fs::write(self.root.join(rel), body).unwrap();
    }

    fn run_in(&self, bin: &str, dir: &str, args: &[&str]) -> Outcome {
        let out = Command::new(bin)
            .args(args)
            .current_dir(self.root.join(dir))
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1112911993 +0000")
            .env("GIT_COMMITTER_DATE", "1112911993 +0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat")
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn git(&self, bin: &str, args: &[&str]) {
        let out = self.run_in(bin, "", args);
        assert_eq!(out.0, 0, "setup `git {args:?}` failed: {out:?}");
    }
}

/// Builds a fixture per binary, runs `setup` in its root, then each of `steps`
/// from `a/`, and returns every step's outcome followed by the final status.
fn both(tag: &str, setup: &[&[&str]], staged: &[(&str, &str)], steps: &[&[&str]]) -> Option<[Vec<Outcome>; 2]> {
    let stock = stock_git()?;
    let run = |bin: &str, side: &str| {
        let f = Fixture::new(bin, &format!("{tag}-{side}"));
        for (path, body) in staged {
            f.write(path, body);
        }
        for args in setup {
            f.git(bin, args);
        }
        let mut outs: Vec<Outcome> = steps.iter().map(|args| f.run_in(bin, "a", args)).collect();
        outs.push(f.run_in(bin, "", &["status", "--porcelain"]));
        outs
    };
    Some([run(stock, "stock"), run(BIN, "zvcs")])
}

#[test]
fn merge_refused_over_staged_changes_restores_them_quietly() {
    let Some([stock, zvcs]) = both(
        "merge",
        &[&["add", "b/y", "b/new"]],
        &[("b/y", "staged\n"), ("b/new", "n\n")],
        &[&["merge", "side"]],
    ) else {
        return;
    };
    assert_eq!(zvcs, stock);
}

#[test]
fn merge_with_two_strategies_rewinds_staged_and_unstaged_changes() {
    let Some([stock, zvcs]) = both(
        "rewind",
        &[&["add", "b/y"]],
        &[("b/y", "staged\n"), ("a/x", "dirty\n")],
        &[&["merge", "-s", "ort", "-s", "resolve", "side"]],
    ) else {
        return;
    };
    assert_eq!(zvcs, stock);
}

#[test]
fn stash_apply_and_pop_index_from_a_subdirectory() {
    let Some([stock, zvcs]) = both(
        "apply",
        &[&["add", "b/y", "b/new"], &["stash", "push", "-q"]],
        &[("b/y", "staged\n"), ("b/new", "n\n")],
        &[&["stash", "apply", "--index"], &["reset", "-q", "--hard"], &["stash", "pop", "--index"]],
    ) else {
        return;
    };
    assert_eq!(zvcs, stock);
}
