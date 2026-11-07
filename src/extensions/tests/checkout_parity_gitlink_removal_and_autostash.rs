//! `git checkout` / `switch` removals, `-` and `-m`'s autostash, against stock git 2.56.
//!
//! * `unlink_entry()` removes through `remove_or_warn()`, which rmdirs a gitlink and warns
//!   when it cannot (entry.c:200-203, wrapper.c:518-527): leaving a branch that has a
//!   submodule whose checkout is still there is `warning: unable to rmdir 'sm': Directory
//!   not empty`.
//! * A `-` or `@{-1}` that resolves to nothing falls back to a pathspec quoting the operand as
//!   typed (builtin/checkout.c:1495-1498).
//! * `-m`'s autostash is `git stash create` plus `reset_head()`: a child that cannot stash
//!   makes the switch die `Cannot autostash` (sequencer.c:4751-4758), and the second
//!   `merge_working_tree()` starts from the reset index, so a staged new file survives a
//!   conflicting re-apply (builtin/checkout.c:1218-1242).
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git_at_least;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// One fixture directory per binary; stock git builds it, the binary under test runs the case.
struct Repo {
    root: PathBuf,
    work: PathBuf,
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
    fn new(tag: &str, label: &str, stock: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-checkout-removal-autostash-{tag}-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("work")).unwrap();
        let root = root.canonicalize().unwrap();
        let work = root.join("work");
        let repo = Repo { root, work, stock: stock.to_owned() };
        repo.git(&["init", "-q", "-b", "main", "."]);
        repo
    }

    fn exec(&self, bin: &str, dir: &Path, args: &[&str], input: Option<&str>) -> Ran {
        let mut child = Command::new(bin)
            .args(args)
            .current_dir(dir)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("ZVCS_HOME", &self.root)
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(text) = input {
            use std::io::Write as _;
            child.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
        }
        let out = child.wait_with_output().unwrap();
        let root = self.root.to_string_lossy().into_owned();
        Ran {
            stdout: String::from_utf8_lossy(&out.stdout).replace(&root, "<root>"),
            stderr: String::from_utf8_lossy(&out.stderr).replace(&root, "<root>"),
            code: out.status.code(),
        }
    }

    /// Stock git, for building the fixture and reading the result back.
    fn git(&self, args: &[&str]) -> String {
        let r = self.exec(&self.stock, &self.work, args, None);
        assert_eq!(r.code, Some(0), "stock git {args:?}: {}", r.stderr);
        r.stdout
    }

    fn write(&self, rel: &str, body: &str) {
        let path = self.work.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    /// Refs, `HEAD`'s reflog, `ORIG_HEAD`, the index and the worktree as stock reads them.
    fn state(&self) -> Vec<String> {
        let mut files: Vec<String> = Vec::new();
        let mut stack = vec![self.work.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.file_name().is_some_and(|n| n == ".git") {
                    continue;
                }
                if path.is_dir() {
                    stack.push(path);
                } else {
                    let rel = path.strip_prefix(&self.work).unwrap().display().to_string();
                    files.push(format!("{rel}: {}", std::fs::read_to_string(&path).unwrap_or_default()));
                }
            }
        }
        files.sort();
        let orig_head = std::fs::read_to_string(self.work.join(".git/ORIG_HEAD")).unwrap_or_default();
        vec![
            self.git(&["for-each-ref", "--format=%(refname) %(objectname)"]),
            self.exec(&self.stock, &self.work, &["log", "-g", "--format=%H %gs", "HEAD"], None).stdout,
            orig_head,
            self.git(&["status", "--porcelain=v2", "--branch", "-uall"]),
            self.git(&["ls-files", "-s"]),
            files.join(""),
        ]
    }
}

/// Build the fixture once per binary with stock git, run `case` under each, and compare
/// what the case printed and what it left behind.
fn compare(tag: &str, setup: impl Fn(&Repo), case: impl Fn(&Repo, &str) -> Vec<Ran>) {
    let Some(stock) = stock_git_at_least((2, 56, 0)) else { return };
    let run = |label: &str, bin: &str| {
        let repo = Repo::new(tag, label, stock);
        setup(&repo);
        let ran = case(&repo, bin);
        (ran, repo.state())
    };
    assert_eq!(run("zvcs", BIN), run("stock", stock), "{tag}");
}

/// `main`: `a`, `b`, `dir/c`, `dir/sub/d` and `.gitignore`, then `a` and `b` edited in two
/// more commits; `side` branches off `main~1` and rewrites `b` and adds `s`; `v1` tags `main~1`.
fn base(r: &Repo) {
    r.write("a", "a1\na2\na3\na4\na5\n");
    r.write("b", "b1\n");
    r.write("dir/c", "c\n");
    r.write("dir/sub/d", "d\n");
    r.write(".gitignore", "*.log\n");
    r.git(&["add", "-A"]);
    r.git(&["commit", "-q", "-m", "one"]);
    r.write("a", "a1\na2\na3\na4\na5\na6\n");
    r.git(&["commit", "-q", "-am", "two"]);
    r.git(&["branch", "side"]);
    r.write("b", "b1\nb2\n");
    r.git(&["commit", "-q", "-am", "three"]);
    r.git(&["checkout", "-q", "side"]);
    r.write("b", "b-side\n");
    r.write("s", "s\n");
    r.git(&["add", "b", "s"]);
    r.git(&["commit", "-q", "-m", "side1"]);
    r.git(&["checkout", "-q", "main"]);
    r.git(&["tag", "v1", "HEAD~1"]);
}

fn git_cmd(r: &Repo, bin: &str, args: &[&str]) -> Vec<Ran> {
    vec![r.exec(bin, &r.work, args, None)]
}

/// `main` gains a submodule `sm` the `side` branch does not have.
fn with_submodule(r: &Repo) {
    base(r);
    let sub = r.root.join("subsrc");
    std::fs::create_dir_all(&sub).unwrap();
    for args in [&["init", "-q", "-b", "main", "."][..], &["commit", "-q", "--allow-empty", "-m", "s1"]] {
        let ran = r.exec(&r.stock, &sub, args, None);
        assert_eq!(ran.code, Some(0), "{}", ran.stderr);
    }
    // Relative to the superproject, so `.gitmodules` is the same in both fixtures.
    r.git(&["-c", "protocol.file.allow=always", "submodule", "-q", "add", "../subsrc", "sm"]);
    r.git(&["commit", "-q", "-m", "addsub"]);
}

#[test]
fn a_populated_submodule_left_behind_is_warned_about() {
    compare("sub-checkout", with_submodule, |r, bin| git_cmd(r, bin, &["checkout", "side"]));
    compare("sub-switch", with_submodule, |r, bin| git_cmd(r, bin, &["switch", "--detach", "side"]));
    compare("sub-orphan", with_submodule, |r, bin| git_cmd(r, bin, &["switch", "--orphan", "o"]));
    compare("sub-restore", with_submodule, |r, bin| {
        git_cmd(r, bin, &["restore", "-s", "side", "--no-overlay", "."])
    });
}

#[test]
fn a_previous_branch_that_is_gone_is_a_pathspec_as_typed() {
    let setup = |r: &Repo| {
        base(r);
        r.git(&["branch", "-q", "-D", "side"]);
    };
    compare("dash", setup, |r, bin| git_cmd(r, bin, &["checkout", "-"]));
    compare("at", setup, |r, bin| git_cmd(r, bin, &["checkout", "@{-1}"]));
}

#[test]
fn merge_autostash_failures_and_staged_additions() {
    let ita = |r: &Repo| {
        base(r);
        r.write("m", "m\n");
        r.git(&["add", "-N", "m"]);
        r.write("a", "A\n");
    };
    compare("ita-checkout", ita, |r, bin| git_cmd(r, bin, &["checkout", "-m", "side"]));
    compare("ita-switch", ita, |r, bin| git_cmd(r, bin, &["switch", "--conflict=diff3", "side"]));
    let staged = |r: &Repo| {
        base(r);
        r.write("n", "new\n");
        r.write("b", "b1\nb2\nst\n");
        r.git(&["add", "n", "b"]);
    };
    compare("staged-checkout", staged, |r, bin| git_cmd(r, bin, &["checkout", "-m", "side"]));
    compare("staged-switch", staged, |r, bin| git_cmd(r, bin, &["switch", "-m", "side"]));
}
