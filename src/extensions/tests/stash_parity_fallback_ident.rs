//! The commits `git stash` makes when no identity is configured.
//!
//! `do_create_stash()` opens with `prepare_fallback_ident("git stash", "git@stash")`
//! (builtin/stash.c:1521), which sets `GIT_{AUTHOR,COMMITTER}_{NAME,EMAIL}` for every
//! field neither the configuration nor the environment gives (ident.c:689-707). A
//! stash made with nothing configured is therefore `git stash <git@stash>`, and one
//! made with only `user.name` is `<name> <git@stash>`; `EMAIL` does not count. The
//! variables stay set for the rest of the process, so `stash push` logs its
//! `refs/stash` entry and the reset after it under the same identity. A merge,
//! rebase or `checkout -m` gets the fallback only for its autostash, through the
//! `git stash create` child it runs. zvcs used the account's synthesized identity,
//! so every such commit id differed.
//!
//! Each case builds one repository with stock git, copies it, and runs the command
//! under both binaries with the identity variables removed and the dates pinned.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

const IDENT_VARS: [&str; 5] = [
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "EMAIL",
];

struct Fixture {
    root: PathBuf,
    stock: &'static str,
    n: std::cell::Cell<usize>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Exit status, stdout and stderr, plus the stock-read post-state.
#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    code: i32,
    stdout: String,
    stderr: String,
    refs: String,
    status: String,
    stash_log: String,
    head_log: String,
    fsck: String,
    autostash: String,
}

/// One identity setup: config written into the copy, environment for the command.
struct Ident {
    config: &'static [(&'static str, &'static str)],
    env: &'static [(&'static str, &'static str)],
}

const IDENTS: &[Ident] = &[
    Ident { config: &[], env: &[] },
    Ident { config: &[("user.name", "U Name")], env: &[] },
    Ident { config: &[("user.email", "u@example.com")], env: &[] },
    Ident { config: &[("author.name", "A Name")], env: &[] },
    Ident { config: &[("committer.email", "c@example.com")], env: &[] },
    Ident { config: &[("user.useConfigOnly", "true")], env: &[] },
    Ident { config: &[], env: &[("GIT_COMMITTER_NAME", "Env C")] },
    Ident { config: &[], env: &[("GIT_AUTHOR_EMAIL", "env-a@example.com")] },
    Ident { config: &[], env: &[("EMAIL", "mail@example.com")] },
];

impl Fixture {
    /// `main` (`i`, `two` adding `g`), `other` at `i`, `div` from `i` adding `h`,
    /// and `fm` one ahead of `main` with a change to `f`.
    fn new(tag: &str) -> Option<Self> {
        let stock = stock_git::stock_git_at_least((2, 56, 0))?;
        let root = std::env::temp_dir().join(format!("zvcs-stash-fallback-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("seed")).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let f = Fixture { root, stock, n: std::cell::Cell::new(0) };
        let seed = f.root.join("seed");
        let git = |args: &[&str]| f.ok(&seed, args);
        git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(seed.join("f"), "a\n").unwrap();
        git(&["add", "f"]);
        git(&["commit", "-q", "-m", "i"]);
        git(&["branch", "other"]);
        git(&["branch", "div"]);
        std::fs::write(seed.join("g"), "b\n").unwrap();
        git(&["add", "g"]);
        git(&["commit", "-q", "-m", "two"]);
        git(&["checkout", "-q", "-b", "fm"]);
        std::fs::write(seed.join("f"), "a\nmain\n").unwrap();
        git(&["commit", "-q", "-a", "-m", "fm"]);
        git(&["checkout", "-q", "div"]);
        std::fs::write(seed.join("h"), "h\n").unwrap();
        git(&["add", "h"]);
        git(&["commit", "-q", "-m", "h"]);
        git(&["checkout", "-q", "other"]);
        Some(f)
    }

    /// A command with an identity, as the seed is built.
    fn cmd(&self, bin: &str, dir: &Path, args: &[&str]) -> Command {
        let mut c = Command::new(bin);
        c.args(args)
            .current_dir(dir)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.root)
            .env("ZVCS_HOME", self.root.join(".zvcs"))
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC");
        c
    }

    fn ok(&self, dir: &Path, args: &[&str]) {
        let out = self.cmd(self.stock, dir, args).output().unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn stock_out(&self, dir: &Path, args: &[&str]) -> String {
        let out = self.cmd(self.stock, dir, args).output().unwrap();
        let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
        s.push_str(&String::from_utf8_lossy(&out.stderr));
        s
    }

    /// Copy the seed, check out `branch`, dirty `f`, apply `ident.config`, then run
    /// `args` under `bin` with every identity variable removed and `ident.env` set.
    fn outcome(&self, bin: &str, n: usize, branch: &str, ident: &Ident, args: &[&str]) -> Outcome {
        let dir = self.root.join(format!("{}-{n}", if bin == ZVCS { "zvcs" } else { "stock" }));
        let cp = Command::new("/bin/cp").arg("-Rp").arg(self.root.join("seed")).arg(&dir).status().unwrap();
        assert!(cp.success());
        self.ok(&dir, &["checkout", "-q", branch]);
        let mut f = std::fs::read(dir.join("f")).unwrap();
        f.extend_from_slice(b"local\n");
        std::fs::write(dir.join("f"), f).unwrap();
        std::fs::write(dir.join("u"), "untracked\n").unwrap();
        for (k, v) in ident.config {
            self.ok(&dir, &["config", k, v]);
        }
        let mut c = self.cmd(bin, &dir, args);
        for k in IDENT_VARS {
            c.env_remove(k);
        }
        for (k, v) in ident.env {
            c.env(k, v);
        }
        let out = c.output().unwrap();
        let root = self.root.to_string_lossy().into_owned();
        let clean = |b: &[u8]| String::from_utf8_lossy(b).replace(&root, "<root>");
        Outcome {
            code: out.status.code().expect("no signal"),
            stdout: clean(&out.stdout),
            stderr: clean(&out.stderr),
            refs: self.stock_out(&dir, &["for-each-ref"]),
            status: self.stock_out(&dir, &["status", "--porcelain"]),
            stash_log: self.stock_out(&dir, &["log", "-g", "--format=%H %an <%ae> %cn <%ce> %gn <%ge> %gs", "refs/stash", "--"]),
            head_log: self.stock_out(&dir, &["reflog", "show", "-2", "--format=%gn <%ge> %gs", "HEAD", "--"]),
            fsck: self.stock_out(&dir, &["fsck", "--no-progress"]),
            autostash: self.stock_out(&dir, &["cat-file", "-p", "MERGE_AUTOSTASH"]),
        }
    }

    #[track_caller]
    fn same(&self, branch: &str, ident: &Ident, args: &[&str]) -> Outcome {
        let n = self.n.get();
        self.n.set(n + 1);
        let stock = self.outcome(self.stock, n, branch, ident, args);
        let zvcs = self.outcome(ZVCS, n, branch, ident, args);
        assert_eq!(
            zvcs, stock,
            "git {args:?} on {branch} with {:?} / {:?}: left is zvcs, right is stock",
            ident.config, ident.env
        );
        stock
    }
}

/// `stash create` prints the `W` id, and that id is the identity: every partial setup
/// keeps what it gives and takes `git stash`/`git@stash` for the rest.
#[test]
fn stash_create_takes_the_fallback_for_each_field_not_given() {
    let Some(f) = Fixture::new("create") else { return };
    for ident in IDENTS {
        let got = f.same("other", ident, &["stash", "create"]);
        assert_eq!(got.code, 0);
        f.same("other", ident, &["stash", "create", "a", "message"]);
    }
    let got = f.same("other", &IDENTS[0], &["-c", "user.name=Cli", "stash", "create"]);
    assert_eq!(got.code, 0);
}

/// `stash push` stores under the fallback, so the `refs/stash` entry and the reset's
/// `HEAD` entry carry it as well as the commits.
#[test]
fn stash_push_logs_under_the_fallback_too() {
    let Some(f) = Fixture::new("push") else { return };
    for ident in IDENTS {
        let got = f.same("other", ident, &["stash", "push", "-q"]);
        assert_eq!(got.code, 0);
        f.same("other", ident, &["stash", "push", "-q", "-u"]);
    }
    f.same("other", &IDENTS[0], &["stash"]);
    f.same("other", &IDENTS[1], &["stash", "save", "-q", "msg"]);
}

/// What the fallback cannot fill still dies in `commit_tree()`'s strict idents
/// (commit.c:1710-1714): a configured but empty name, an empty `GIT_AUTHOR_NAME`, a
/// name made only of crud.
#[test]
fn a_given_but_unusable_name_still_dies() {
    let Some(f) = Fixture::new("strict") else { return };
    let empty_user = Ident { config: &[("user.name", "")], env: &[] };
    let empty_env = Ident { config: &[], env: &[("GIT_AUTHOR_NAME", "")] };
    let crud = Ident { config: &[], env: &[("GIT_COMMITTER_NAME", ";;")] };
    for ident in [&empty_user, &empty_env, &crud] {
        let got = f.same("other", ident, &["stash", "create"]);
        assert_eq!(got.code, 128);
        f.same("other", ident, &["stash", "push"]);
    }
}

/// A merge's `MERGE_AUTOSTASH` comes from its `git stash create` child: under the
/// fallback when the merge itself then refuses the committer identity, and a child
/// that dies is followed by `fatal: Cannot autostash`. A `committer.email` alone
/// leaves the merge commit's author on `ident_default_email()`, which is empty.
#[test]
fn merge_autostash_is_made_under_the_fallback() {
    let Some(f) = Fixture::new("merge") else { return };
    for ident in IDENTS {
        f.same("div", ident, &["merge", "--autostash", "main"]);
        f.same("other", ident, &["merge", "--autostash", "main"]);
    }
    let empty_env = Ident { config: &[], env: &[("GIT_AUTHOR_NAME", "")] };
    for branch in ["other", "div"] {
        let got = f.same(branch, &empty_env, &["merge", "--autostash", "main"]);
        assert_eq!(got.code, 128);
    }
}

/// `checkout -m` whose re-apply conflicts keeps the autostash in `refs/stash`: the
/// commit is the child's, under the fallback, and the reflog entry is the parent's,
/// under the account's identity.
#[test]
fn checkout_m_keeps_a_fallback_autostash() {
    let Some(f) = Fixture::new("checkout") else { return };
    for ident in IDENTS {
        let got = f.same("other", ident, &["checkout", "-m", "fm"]);
        assert_eq!(got.code, 0);
    }
}
