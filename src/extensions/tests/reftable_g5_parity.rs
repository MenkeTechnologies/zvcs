//! Readers of reflogs and `HEAD` outside the ref plumbing — `@{...}` revision
//! syntax, `show-branch --reflog`, `merge-base --fork-point`, `fast-export
//! --reflog`, `repo structure` and command-line `includeIf.onbranch:` — in
//! repositories whose references live in reftables, each next to its `files`
//! twin. Every repository is built by stock git and every expectation is what
//! stock git prints for the same command.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    stock: &'static str,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn run(bin: &str, dir: &Path, root: &Path, args: &[&str], date: &str) -> Output {
    Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root)
        .env("ZVCS_HOME", root.join(".zvcs"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .output()
        .unwrap()
}

impl Fixture {
    fn new(tag: &str) -> Option<Self> {
        let stock = stock_git::stock_git_at_least((2, 56, 0))?;
        let root = std::env::temp_dir().join(format!("zvcs-reftable-g5-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Some(Fixture { root, stock })
    }

    /// Run stock git at `date` (seconds since the epoch) and require success.
    fn stock_at(&self, dir: &str, args: &[&str], date: u64) {
        let out = run(self.stock, &self.root.join(dir), &self.root, args, &format!("{date} +0100"));
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// `R<fmt>`: three commits on `main`, `side` branched from the second and
    /// advanced once on its own, an annotated tag, and the checkouts between
    /// the two branches recorded in HEAD's reflog; `R<fmt>-wt` is a linked
    /// worktree on `side` with a commit of its own.
    fn repo(&self, format: &str) -> String {
        let name = format!("R{format}");
        self.stock_at(".", &["init", "-q", "-b", "main", &format!("--ref-format={format}"), &name], 1_700_000_000);
        let mut date = 1_700_000_000;
        let mut at = |args: &[&str], dir: &str| {
            date += 100;
            self.stock_at(dir, args, date);
        };
        for msg in ["one", "two", "three"] {
            at(&["commit", "-q", "--allow-empty", "-m", msg], &name);
        }
        at(&["branch", "side", "HEAD~1"], &name);
        at(&["tag", "-a", "v1", "-m", "t"], &name);
        at(&["checkout", "-q", "side"], &name);
        at(&["commit", "-q", "--allow-empty", "-m", "s1"], &name);
        at(&["checkout", "-q", "main"], &name);
        at(&["checkout", "-q", "-b", "wtb", "side"], &name);
        at(&["checkout", "-q", "main"], &name);
        at(&["worktree", "add", "-q", &format!("../{name}-wt"), "wtb"], &name);
        let wt = format!("{name}-wt");
        at(&["commit", "-q", "--allow-empty", "-m", "w"], &wt);
        // A commit only the linked worktree's own HEAD log remembers.
        at(&["checkout", "-q", "--detach"], &wt);
        at(&["commit", "-q", "--allow-empty", "-m", "detached"], &wt);
        at(&["checkout", "-q", "wtb"], &wt);
        name
    }

    /// Run `args` in `dir` under stock git and zvcs and require the same
    /// stdout, stderr and exit status.
    fn same(&self, dir: &str, args: &[&str]) {
        let dir = self.root.join(dir);
        let date = "1700009000 +0100";
        let stock = run(self.stock, &dir, &self.root, args, date);
        let zvcs = run(BIN, &dir, &self.root, args, date);
        assert_eq!(
            (
                String::from_utf8_lossy(&zvcs.stdout),
                String::from_utf8_lossy(&zvcs.stderr),
                zvcs.status.code()
            ),
            (
                String::from_utf8_lossy(&stock.stdout),
                String::from_utf8_lossy(&stock.stderr),
                stock.status.code()
            ),
            "{args:?} in {}",
            dir.display()
        );
    }
}

/// `get_oid_basic()`'s reflog branch finds the log through `repo_dwim_log()`,
/// whose `refs_reflog_exists()` is the backend's: reftable reflogs are records,
/// not `logs/` files. Covers the n-th entry, dates, epoch timestamps, `@{-n}`,
/// out-of-range diagnostics and the `main-worktree/` / `worktrees/<id>/` spellings.
#[test]
fn reflog_selectors_resolve_in_both_formats() {
    let Some(fx) = Fixture::new("sel") else { return };
    for format in ["reftable", "files"] {
        let repo = fx.repo(format);
        let wt = format!("{repo}-wt");
        for spec in [
            "HEAD@{0}",
            "HEAD@{1}",
            "@{1}",
            "@{-1}",
            "@{-2}@{1}",
            "main@{1}",
            "refs/heads/main@{0}",
            "heads/side@{1}",
            "main@{2.days.ago}",
            "main@{1700000250}",
            "main@{1600000000}",
            "main@{1}^{tree}",
            "HEAD@{1}~1",
            "HEAD@{20}",
            "main@{3}",
            "v1@{0}",
            "nosuch@{0}",
        ] {
            fx.same(&repo, &["rev-parse", spec]);
        }
        for spec in ["HEAD@{1}", "main-worktree/HEAD@{2}", &format!("worktrees/{wt}/HEAD@{{1}}")] {
            fx.same(&wt, &["rev-parse", spec]);
            fx.same(&repo, &["rev-parse", spec]);
        }
        fx.same(&repo, &["rev-parse", "--symbolic-full-name", "@{-1}", "HEAD@{1}"]);
    }
}

/// `show-branch --reflog` and `merge-base --fork-point` walk reflogs through
/// the same lookup.
#[test]
fn reflog_walkers_match_stock() {
    let Some(fx) = Fixture::new("walk") else { return };
    for format in ["reftable", "files"] {
        let repo = fx.repo(format);
        fx.same(&repo, &["show-branch", "--reflog"]);
        fx.same(&repo, &["show-branch", "--reflog=3,1", "side"]);
        fx.same(&repo, &["show-branch", "-g=2", "HEAD"]);
        fx.same(&repo, &["merge-base", "--fork-point", "main", "side"]);
        fx.same(&repo, &["merge-base", "--fork-point", "side"]);
    }
}

/// `add_reflogs_to_pending()` walks every reflog of every worktree, which in
/// the main worktree includes the linked one's `HEAD` log and the detached
/// commit no reference reaches.
#[test]
fn fast_export_reflog_walks_every_worktree() {
    let Some(fx) = Fixture::new("fe") else { return };
    for format in ["reftable", "files"] {
        let repo = fx.repo(format);
        fx.same(&repo, &["fast-export", "--reflog"]);
        fx.same(&repo, &["fast-export", "--reflog", "--all"]);
        fx.same(&format!("{repo}-wt"), &["fast-export", "--reflog", "main"]);
    }
}

/// `repo structure` in a linked worktree reads objects from the shared object
/// directory instead of `<worktree git dir>/objects`.
#[test]
fn repo_structure_counts_from_a_linked_worktree() {
    let Some(fx) = Fixture::new("repo") else { return };
    for format in ["reftable", "files"] {
        let repo = fx.repo(format);
        let main = run(BIN, &fx.root.join(&repo), &fx.root, &["repo", "structure", "--format=lines"], "0 +0000");
        let linked = run(BIN, &fx.root.join(format!("{repo}-wt")), &fx.root, &["repo", "structure", "--format=lines"], "0 +0000");
        assert!(linked.status.success(), "{}", String::from_utf8_lossy(&linked.stderr));
        assert_eq!(linked.stdout, main.stdout);
        fx.same(&repo, &["repo", "info", "references.format"]);
    }
}

/// `include_by_branch()` resolves `HEAD` through the ref store; in a reftable
/// repository the `HEAD` file is the `refs/heads/.invalid` stub. A relative path
/// behind a true condition is refused, behind a false one never looked at.
#[test]
fn command_line_onbranch_reads_head_from_the_backend() {
    let Some(fx) = Fixture::new("onbranch") else { return };
    for format in ["reftable", "files"] {
        let repo = fx.repo(format);
        for dir in [repo.clone(), format!("{repo}-wt")] {
            for branch in ["main", "wtb", "ma*", ".invalid"] {
                let key = format!("includeIf.onbranch:{branch}.path=rel");
                fx.same(&dir, &["-c", &key, "config", "foo.bar"]);
            }
        }
    }
}
