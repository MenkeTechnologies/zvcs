//! `git merge` with a committer identity `fmt_ident()` refuses.
//!
//! `cmd_merge` calls `git_committer_info(IDENT_STRICT)` twice and throws the
//! result away: ahead of `Trying really trivial in-index merge...`
//! (builtin/merge.c:1722) and, for every merge that runs a strategy, after
//! `create_autostash_ref()` and before `save_state()` (builtin/merge.c:1763-1764).
//! An unusable identity therefore dies there with `fmt_ident()`'s advice block
//! and `fatal:` line, before a strategy touches the index — no `MERGE_HEAD`, no
//! conflict stages, no commit. zvcs synthesized a reflog identity and merged.
//!
//! Each case builds one repository with stock git, copies it, and runs the
//! command under both binaries with the committer variables removed and the
//! dates pinned.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    stock: &'static str,
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
    merge_head: bool,
}

impl Fixture {
    /// `main` (`i`, `m`), `clean` forked from `i` with a new file, `side`
    /// forked from `i` with a conflicting edit of `f`, `ff` one ahead of `main`.
    fn new(tag: &str) -> Option<Self> {
        let stock = stock_git::stock_git_at_least((2, 56, 0))?;
        let root = std::env::temp_dir().join(format!("zvcs-merge-ident-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("seed")).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let f = Fixture { root, stock };
        let seed = f.root.join("seed");
        let git = |args: &[&str]| f.ok(stock, &seed, args);
        git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(seed.join("f"), "b\n").unwrap();
        git(&["add", "f"]);
        git(&["commit", "-q", "-m", "i"]);
        git(&["branch", "side"]);
        git(&["branch", "clean"]);
        std::fs::write(seed.join("f"), "d\n").unwrap();
        git(&["commit", "-q", "-a", "-m", "m"]);
        git(&["checkout", "-q", "-b", "ff"]);
        std::fs::write(seed.join("g"), "z\n").unwrap();
        git(&["add", "g"]);
        git(&["commit", "-q", "-m", "ff"]);
        git(&["checkout", "-q", "side"]);
        std::fs::write(seed.join("f"), "c\n").unwrap();
        git(&["commit", "-q", "-a", "-m", "s"]);
        git(&["checkout", "-q", "clean"]);
        std::fs::write(seed.join("q"), "q\n").unwrap();
        git(&["add", "q"]);
        git(&["commit", "-q", "-m", "q"]);
        git(&["checkout", "-q", "main"]);
        Some(f)
    }

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

    fn ok(&self, bin: &str, dir: &Path, args: &[&str]) {
        let out = self.cmd(bin, dir, args).output().unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn stock_out(&self, dir: &Path, args: &[&str]) -> String {
        String::from_utf8_lossy(&self.cmd(self.stock, dir, args).output().unwrap().stdout).into_owned()
    }

    /// Copy the seed to `<bin>-<n>`, apply `config`, then run `args` there
    /// under `bin` with the environment adjusted by `env`.
    fn outcome(&self, bin: &str, n: usize, config: &[(&str, &str)], env: &Env, args: &[&str]) -> Outcome {
        let dir = self.root.join(format!("{}-{n}", if bin == ZVCS { "zvcs" } else { "stock" }));
        let cp = Command::new("/bin/cp").arg("-Rp").arg(self.root.join("seed")).arg(&dir).status().unwrap();
        assert!(cp.success());
        for (k, v) in config {
            self.ok(self.stock, &dir, &["config", k, v]);
        }
        let mut c = self.cmd(bin, &dir, args);
        for k in env.remove {
            c.env_remove(k);
        }
        for (k, v) in env.set {
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
            merge_head: dir.join(".git/MERGE_HEAD").exists(),
        }
    }

    #[track_caller]
    fn same(&self, n: usize, config: &[(&str, &str)], env: &Env, args: &[&str]) -> Outcome {
        let stock = self.outcome(self.stock, n, config, env, args);
        let zvcs = self.outcome(ZVCS, n, config, env, args);
        assert_eq!(zvcs, stock, "git {args:?} with {config:?}: left is zvcs, right is stock");
        stock
    }
}

struct Env {
    remove: &'static [&'static str],
    set: &'static [(&'static str, &'static str)],
}

const NO_COMMITTER: Env = Env {
    remove: &["GIT_COMMITTER_NAME", "GIT_COMMITTER_EMAIL", "EMAIL"],
    set: &[],
};

const CONFIG_ONLY: &[(&str, &str)] = &[("user.useConfigOnly", "true")];

/// `user.useConfigOnly` with no email anywhere: every merge that would make a
/// commit dies at the strict check, whatever strategy or flag got it there.
#[test]
fn no_email_under_use_config_only_dies_before_the_strategy() {
    let Some(f) = Fixture::new("noemail") else { return };
    let cases: &[&[&str]] = &[
        &["merge", "-m", "x", "clean"],
        &["merge", "-m", "x", "side"],
        &["merge", "--no-ff", "-m", "x", "ff"],
        &["merge", "--no-commit", "clean"],
        &["merge", "--squash", "clean"],
        &["merge", "-s", "ours", "-m", "x", "side"],
        &["merge", "-s", "resolve", "-m", "x", "clean"],
        &["merge", "-q", "-m", "x", "clean", "side"],
    ];
    for (n, args) in cases.iter().enumerate() {
        let got = f.same(n, CONFIG_ONLY, &NO_COMMITTER, args);
        assert_eq!(got.code, 128, "{args:?}");
        assert!(!got.merge_head, "{args:?}");
    }
    // A fast-forward makes no commit and never asks.
    let got = f.same(cases.len(), CONFIG_ONLY, &NO_COMMITTER, &["merge", "ff"]);
    assert_eq!(got.code, 0);
}

/// The name half, the `*.email` keys that count as given, and the refusals
/// that need no `user.useConfigOnly`: an empty name, a name of crud only, and
/// a committer date `parse_date()` rejects.
#[test]
fn each_refusal_of_fmt_ident_matches() {
    let Some(f) = Fixture::new("variants") else { return };
    let merge: &[&str] = &["merge", "-m", "x", "clean"];
    let trivial: &[&str] = &["merge", "-s", "resolve", "-m", "x", "clean"];
    let mut n = 0;
    let mut same = |config: &[(&str, &str)], env: &Env| {
        for args in [merge, trivial] {
            f.same(n, config, env, args);
            n += 1;
        }
    };
    same(&[("user.useConfigOnly", "true"), ("user.email", "u@example.com")], &NO_COMMITTER);
    same(&[("user.useConfigOnly", "true"), ("author.email", "a@example.com"), ("author.name", "a")], &NO_COMMITTER);
    same(&[("user.useConfigOnly", "true"), ("user.email", ""), ("user.name", "N")], &NO_COMMITTER);
    same(&[("user.useConfigOnly", "true"), ("committer.email", "c@x"), ("committer.name", "")], &NO_COMMITTER);
    same(&[], &Env { remove: &[], set: &[("GIT_COMMITTER_NAME", "")] });
    same(&[], &Env { remove: &[], set: &[("GIT_COMMITTER_NAME", "<>")] });
    same(&[], &Env { remove: &[], set: &[("GIT_COMMITTER_DATE", "garbage")] });
    same(CONFIG_ONLY, &Env { remove: &["GIT_COMMITTER_EMAIL"], set: &[("EMAIL", "e@example.com")] });
}

/// A dirty `--autostash` merge stashes first and dies after: the autostash
/// stays in `MERGE_AUTOSTASH` and the worktree is left clean, as stock leaves it.
#[test]
fn autostash_is_created_before_the_check() {
    let Some(f) = Fixture::new("autostash") else { return };
    std::fs::write(f.root.join("seed/f"), "dirty\n").unwrap();
    // The stash commit itself carries `prepare_fallback_ident()`'s identity,
    // so only the shape is compared, not the id.
    let mut stock = f.outcome(f.stock, 0, CONFIG_ONLY, &NO_COMMITTER, &["merge", "--autostash", "-m", "x", "clean"]);
    let mut zvcs = f.outcome(ZVCS, 0, CONFIG_ONLY, &NO_COMMITTER, &["merge", "--autostash", "-m", "x", "clean"]);
    for o in [&mut stock, &mut zvcs] {
        assert!(o.stdout.starts_with("Created autostash: "), "{o:?}");
        o.stdout.clear();
    }
    assert_eq!(zvcs, stock);
    assert_eq!(stock.code, 128);
    assert!(f.root.join("zvcs-0/.git/MERGE_AUTOSTASH").exists());
}
