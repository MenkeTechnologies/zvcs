//! The pathspec reset of `git stash push`, against stock git.
//!
//! With a pathspec, `do_push_stash()` resets through three children
//! (builtin/stash.c:1781-1815): `git add [-u] [--force] -- <ps>`, then
//! `git diff-index -p --cached --binary HEAD -- <ps>`, then that patch through
//! `git apply --index -R`; any of them failing ends the push with -1 after the stash
//! is stored. The states here are the ones where that differs from "reset the
//! matched paths to `HEAD` and delete the untracked ones":
//!
//! * an untracked path `HEAD` tracks (`git rm --cached`, no index file) is staged
//!   back by `add`; with the same content the patch is empty and `apply` fails
//!   `No valid patches in input`, leaving it staged; with other content it is
//!   reverted to `HEAD`, never deleted;
//! * without `-u`, `add -u` does not stage such a path back, so the reversed deletion
//!   is a creation over a worktree file: `already exists in working directory`;
//! * `add`'s own refusals (a spec matching no file) end the push;
//! * `--keep-index`'s `checkout --no-overlay` matches the pathspec against the index
//!   too, so a path `HEAD` tracks is no `did not match` error there;
//! * the `add` child is handed each spec's normalized `original` (pathspec.c:513-521),
//!   so `./g` and `sub/../f` reach it as `:(prefix:0)g` and `:(prefix:0)f`;
//! * no tree object is written for the index the reset leaves.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git_at_least;

const BIN: &str = env!("CARGO_BIN_EXE_git");
const DATE: &str = "1136214245 +0000";

struct Repo {
    root: PathBuf,
    work: PathBuf,
    bin: String,
    stock: String,
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[derive(Debug, PartialEq)]
struct Ran {
    stdout: String,
    stderr: String,
    code: Option<i32>,
}

impl Repo {
    fn new(tag: &str, bin: &str, stock: &str) -> Self {
        let label = if bin == BIN { "zvcs" } else { "stock" };
        let root = std::env::temp_dir()
            .join(format!("zvcs-stash-pathspec-reset-{tag}-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("work/sub")).unwrap();
        let root = root.canonicalize().unwrap();
        let work = root.join("work");
        let repo = Repo { root, work, bin: bin.to_owned(), stock: stock.to_owned() };
        repo.ok(&["init", "-q", "-b", "main", "."]);
        repo.write("f", "base\n");
        repo.write("g", "base\n");
        repo.write("sub/s", "s\n");
        repo.ok(&["add", "."]);
        repo.ok(&["commit", "-q", "-m", "init"]);
        repo
    }

    fn run_with(&self, bin: &str, args: &[&str]) -> Ran {
        let out = Command::new(bin)
            .args(args)
            .current_dir(&self.work)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("PWD", &self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@e.co")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@e.co")
            .env("GIT_AUTHOR_DATE", DATE)
            .env("GIT_COMMITTER_DATE", DATE)
            .env("ZVCS_HOME", &self.root)
            .output()
            .unwrap();
        let root = self.root.to_string_lossy().into_owned();
        Ran {
            stdout: String::from_utf8_lossy(&out.stdout).replace(&root, "<root>"),
            stderr: String::from_utf8_lossy(&out.stderr).replace(&root, "<root>"),
            code: out.status.code(),
        }
    }

    /// Fixture setup goes through stock git, so both sides start from the same bytes.
    fn ok(&self, args: &[&str]) {
        let r = self.run_with(&self.stock, args);
        assert_eq!(r.code, Some(0), "git {args:?}: {}", r.stderr);
    }

    fn write(&self, rel: &str, body: &str) {
        std::fs::write(self.work.join(rel), body).unwrap();
    }

    /// Everything the push left, read by stock git: the stash, the index, the worktree
    /// against it, every file's content, and `fsck` (a stray tree object is `dangling`).
    fn state(&self) -> Vec<Ran> {
        let mut files = String::new();
        for rel in ["f", "g", "n", "ign", "sub/s"] {
            if let Ok(body) = std::fs::read_to_string(self.work.join(rel)) {
                files.push_str(&format!("{rel}: {body}"));
            }
        }
        vec![
            self.run_with(&self.stock, &["stash", "list"]),
            self.run_with(&self.stock, &["stash", "show", "-p", "--include-untracked", "stash@{0}"]),
            self.run_with(&self.stock, &["ls-files", "-s", "--debug"]),
            self.run_with(&self.stock, &["status", "--porcelain"]),
            self.run_with(&self.stock, &["fsck"]),
            Ran { stdout: files, stderr: String::new(), code: None },
        ]
    }
}

/// `ls-files --debug` prints stat times and the device and inode, which are the run's own;
/// everything else of an entry (flags, size, mode) is compared.
fn without_times(mut state: Vec<Ran>) -> Vec<Ran> {
    state[2].stdout = state[2]
        .stdout
        .lines()
        .filter(|l| !["ctime:", "mtime:", "dev:"].iter().any(|p| l.trim_start().starts_with(p)))
        .map(|l| format!("{l}\n"))
        .collect();
    state
}

/// One fixture state: what to do to the committed `f`, `g`, `sub/s` before the push.
#[derive(Clone, Copy, Debug)]
enum Setup {
    /// `git rm --cached f`: `HEAD` tracks it, the index does not, the worktree has it.
    DropF,
    /// The same, with `f` edited.
    DropFEdited,
    /// The same, plus an edit to `g` and a new file `n`.
    DropFEditG,
    /// No index file at all.
    NoIndex,
    /// `git rm f`: gone from both.
    RemoveF,
    /// `f` and `g` both staged with edits.
    StageBoth,
}

fn prepare(r: &Repo, setup: Setup) {
    match setup {
        Setup::DropF => r.ok(&["rm", "-q", "--cached", "f"]),
        Setup::DropFEdited => {
            r.ok(&["rm", "-q", "--cached", "f"]);
            r.write("f", "mod\n");
        }
        Setup::DropFEditG => {
            r.ok(&["rm", "-q", "--cached", "f"]);
            r.write("g", "mod\n");
            r.write("n", "n\n");
        }
        Setup::NoIndex => std::fs::remove_file(r.work.join(".git/index")).unwrap(),
        Setup::RemoveF => r.ok(&["rm", "-q", "f"]),
        Setup::StageBoth => {
            r.write("f", "mod\n");
            r.write("g", "mod\n");
            r.ok(&["add", "f", "g"]);
        }
    }
}

fn compare(name: &str, cases: &[(Setup, &[&str])]) {
    let Some(stock) = stock_git_at_least((2, 56, 0)) else { return };
    for (i, (setup, args)) in cases.iter().enumerate() {
        let run = |bin: &str| {
            let r = Repo::new(&format!("{name}-{i}"), bin, stock);
            prepare(&r, *setup);
            if args.contains(&"--pathspec-from-file=ps") {
                r.write("ps", "f\n");
            }
            (r.run_with(bin, args), without_times(r.state()))
        };
        assert_eq!(run(BIN), run(stock), "{setup:?} git {args:?}");
    }
}

#[test]
fn an_untracked_path_head_tracks_is_staged_back_and_never_deleted() {
    compare("untracked", &[
        (Setup::DropF, &["stash", "push", "-u", "--", "f"]),
        (Setup::DropF, &["stash", "push", "-a", "--", "f"]),
        (Setup::DropF, &["stash", "push", "-q", "-u", "--", "f"]),
        (Setup::DropF, &["stash", "-u", "--", "f"]),
        (Setup::DropF, &["stash", "push", "-u", "--", "."]),
        (Setup::DropF, &["stash", "push", "-u", "--pathspec-from-file=ps"]),
        (Setup::NoIndex, &["stash", "push", "-u", "--", "f"]),
        (Setup::DropFEdited, &["stash", "push", "-u", "--", "f"]),
        (Setup::DropFEditG, &["stash", "push", "-u", "--", "f", "g", "n"]),
        (Setup::DropFEditG, &["stash", "push", "-u", "--", "./g", "./n", "sub/../f"]),
    ]);
}

#[test]
fn keep_index_matches_its_checkout_against_the_reset_index() {
    compare("keep", &[
        (Setup::DropF, &["stash", "push", "-u", "-k", "--", "f"]),
        (Setup::DropFEdited, &["stash", "push", "-u", "-k", "--", "f"]),
        (Setup::DropFEditG, &["stash", "push", "-u", "-k", "--", "f", "g"]),
        (Setup::NoIndex, &["stash", "push", "-u", "-k", "--", "f", "g"]),
    ]);
}

#[test]
fn a_child_that_refuses_ends_the_push_after_the_stash_is_stored() {
    compare("refuse", &[
        // `add -u` leaves `f` unstaged, so the reversed deletion recreates it over the file.
        (Setup::DropFEditG, &["stash", "push", "--", "."]),
        (Setup::DropFEditG, &["stash", "--", "."]),
        // `add -- f` with `f` nowhere: `pathspec ... did not match any files`.
        (Setup::RemoveF, &["stash", "push", "-u", "--", "f"]),
        (Setup::DropFEditG, &["stash", "push", "-u", "--", "g", "nonexist"]),
        (Setup::DropF, &["stash", "push", "-S", "--", "f", "g"]),
    ]);
}

#[test]
fn the_reset_index_writes_no_tree_object() {
    compare("tree", &[
        (Setup::StageBoth, &["stash", "push", "--", "g"]),
        (Setup::StageBoth, &["stash", "push", "-u", "--", "g"]),
    ]);
}
