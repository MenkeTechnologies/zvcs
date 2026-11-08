//! Two ref-filter lookups, driven through stock git and zvcs on fixtures stock
//! git built.
//!
//! * `%(upstream:remotename)` and `%(upstream:remoteref)` render empty unless
//!   `branch_get_upstream()` names an upstream (ref-filter.c:2486-2501): a
//!   branch with `branch.<name>.remote` but no `merge`, a `merge` but no
//!   `remote`, or a `merge` the remote's fetch refspecs do not map has none.
//!   Only `%(push:remotename)` / `%(push:remoteref)` skip the lookup
//!   (ref-filter.c:2510-2511). zvcs answered the upstream ones regardless.
//! * `%(worktreepath)` looks a branch up in `get_worktrees()`, whose main entry
//!   exists for a bare repository too, its path the repository itself
//!   (worktree.c:120-146): the bare repository's `HEAD` branch has a worktree
//!   path, and `branch` run in a linked worktree of it marks that branch `+`.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");
#[path = "support/stock_git.rs"]
mod stock_git;

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn run(bin: &str, root: &Path, dir: &Path, args: &[&str]) -> Output {
    Command::new(bin)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .env("HOME", root)
        .env("ZVCS_HOME", root.join("zvcs-home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("gitconfig"))
        .env("GIT_EDITOR", "true")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .expect("run binary")
}

impl Fixture {
    /// `repo`: `main` and `side`, a remote `origin` fetching only `main`, and
    /// branches `nomerge` (remote, no merge), `onlymerge` (merge, no remote),
    /// `unfetched` (a merge the refspec does not map), `tracked` (a real
    /// upstream) and `local` (upstream `.`). `bare`: a bare clone of it with a
    /// linked worktree `wt` on a new branch `wtb`.
    fn new(stock: &str, tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-fer-upstream-bare-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("repo")).unwrap();
        std::fs::write(root.join("gitconfig"), "").unwrap();
        let f = Fixture { root };
        f.setup(stock, "repo", &["init", "-q", "-b", "main"]);
        f.setup(stock, "repo", &["-c", "maintenance.auto=false", "commit", "-q", "--allow-empty", "-m", "one"]);
        f.setup(stock, "repo", &["-c", "maintenance.auto=false", "commit", "-q", "--allow-empty", "-m", "two"]);
        for b in ["nomerge", "onlymerge", "unfetched", "tracked", "local"] {
            f.setup(stock, "repo", &["branch", b]);
        }
        for (key, value) in [
            ("remote.origin.url", "../bare"),
            ("remote.origin.fetch", "+refs/heads/main:refs/remotes/origin/main"),
            ("branch.nomerge.remote", "origin"),
            ("branch.onlymerge.merge", "refs/heads/main"),
            ("branch.unfetched.remote", "origin"),
            ("branch.unfetched.merge", "refs/heads/other"),
            ("branch.tracked.remote", "origin"),
            ("branch.tracked.merge", "refs/heads/main"),
            ("branch.local.remote", "."),
            ("branch.local.merge", "refs/heads/main"),
        ] {
            f.setup(stock, "repo", &["config", key, value]);
        }
        f.setup(stock, "repo", &["update-ref", "refs/remotes/origin/main", "HEAD~1"]);
        f.setup(stock, ".", &["-c", "maintenance.auto=false", "clone", "-q", "--bare", "repo", "bare"]);
        f.setup(stock, "bare", &["worktree", "add", "-q", "-b", "wtb", "../wt"]);
        f
    }

    fn setup(&self, stock: &str, dir: &str, args: &[&str]) {
        let out = run(stock, &self.root, &self.root.join(dir), args);
        assert!(out.status.success(), "setup {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// Run `args` in `dir` under both binaries and require identical stdout,
    /// stderr and exit status.
    fn same(&self, stock: &str, dir: &str, args: &[&str]) {
        let dir = self.root.join(dir);
        let want = run(stock, &self.root, &dir, args);
        let got = run(BIN, &self.root, &dir, args);
        assert_eq!(
            (String::from_utf8_lossy(&got.stdout), String::from_utf8_lossy(&got.stderr), got.status.code()),
            (String::from_utf8_lossy(&want.stdout), String::from_utf8_lossy(&want.stderr), want.status.code()),
            "{args:?}"
        );
    }
}

#[test]
fn upstream_remote_options_need_an_upstream() {
    let Some(stock) = stock_git::stock_git() else { return };
    let f = Fixture::new(stock, "upstream");
    for format in [
        "%(upstream:remotename)|%(upstream:remoteref)|%(refname)",
        "%(upstream)|%(upstream:short)|%(upstream:track)|%(refname)",
        "%(push:remotename)|%(push:remoteref)|%(refname)",
        "%(if)%(upstream:remotename)%(then)R%(else)-%(end)%(refname)",
    ] {
        f.same(stock, "repo", &["for-each-ref", &format!("--format={format}"), "refs/heads"]);
    }
    f.same(stock, "repo", &["branch", "--format=%(upstream:remoteref)%(upstream:remotename)"]);
    f.same(stock, "repo", &["for-each-ref", "--sort=upstream:remotename", "--format=%(refname)", "refs/heads"]);
}

#[test]
fn a_bare_repository_is_its_head_branch_worktree() {
    let Some(stock) = stock_git::stock_git() else { return };
    let f = Fixture::new(stock, "bare");
    for dir in ["bare", "wt"] {
        f.same(stock, dir, &["for-each-ref", "--format=%(refname) %(HEAD) %(worktreepath)", "refs/heads"]);
        f.same(stock, dir, &["branch", "-v"]);
        f.same(stock, dir, &["-c", "color.branch.worktree=cyan", "branch", "--color=always"]);
        f.same(stock, dir, &["-c", "color.branch=always", "branch"]);
    }
}
