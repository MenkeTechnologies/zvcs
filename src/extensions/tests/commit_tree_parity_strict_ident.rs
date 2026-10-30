//! The strict identity lookups `commit_tree()` makes for every commit it writes.
//!
//! `write_commit_tree()` fills a missing author with `git_author_info(IDENT_STRICT)`
//! and the committer with `git_committer_info(IDENT_STRICT)` (commit.c:1709-1714), so
//! a field neither the environment nor the configuration gives falls back to the
//! account's default and dies when that default is bogus — a hostname with no domain
//! is `unable to auto-detect email address`, exit 128, under the `Author`/`Committer
//! identity unknown` advice. Each verb dies where git calls it, after whatever it has
//! already written: a merge with `MERGE_HEAD`, `MERGE_MSG` and `MERGE_AUTOSTASH`
//! staged, a pick with its result applied and `CHERRY_PICK_HEAD` recorded, a revert
//! (whose author is the user's own) with its result applied, a notes commit after
//! its tree. A pick and an `--amend` hand over an author of their own, so only the
//! committer is looked up. zvcs used to synthesize the missing half and commit.
//!
//! Only a machine whose default address *is* bogus reaches the death; elsewhere both
//! binaries commit with the same synthesized identity, which the comparison still
//! checks. Each case builds one repository with stock git, copies it, and runs the
//! command under both binaries with the identity variables removed and the dates
//! pinned.
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

/// Each of the four variables alone, and the pairs that complete one role.
const ENVS: &[&[(&str, &str)]] = &[
    &[("GIT_COMMITTER_EMAIL", "ce@example.com")],
    &[("GIT_COMMITTER_NAME", "Env C")],
    &[("GIT_AUTHOR_EMAIL", "ae@example.com")],
    &[("GIT_AUTHOR_NAME", "Env A")],
    &[("GIT_AUTHOR_NAME", "Env A"), ("GIT_AUTHOR_EMAIL", "ae@example.com")],
    &[("GIT_COMMITTER_NAME", "Env C"), ("GIT_COMMITTER_EMAIL", "ce@example.com")],
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
    log: String,
    state_files: Vec<String>,
    fsck: String,
}

impl Fixture {
    /// `base` adding `f` and `o`, then `side` adding `s` and `main` adding `m`.
    fn new(tag: &str) -> Option<Self> {
        let stock = stock_git::stock_git_at_least((2, 56, 0))?;
        let root = std::env::temp_dir().join(format!("zvcs-strict-ident-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("seed")).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let f = Fixture { root, stock, n: std::cell::Cell::new(0) };
        let seed = f.root.join("seed");
        let git = |args: &[&str]| f.ok(&seed, args);
        git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(seed.join("f"), "base\n").unwrap();
        std::fs::write(seed.join("o"), "o\n").unwrap();
        git(&["add", "f", "o"]);
        git(&["commit", "-q", "-m", "base"]);
        git(&["checkout", "-q", "-b", "side"]);
        std::fs::write(seed.join("s"), "s\n").unwrap();
        git(&["add", "s"]);
        git(&["commit", "-q", "-m", "side"]);
        git(&["checkout", "-q", "main"]);
        std::fs::write(seed.join("m"), "m\n").unwrap();
        git(&["add", "m"]);
        git(&["commit", "-q", "-m", "main"]);
        let patches = f.root.join("patches");
        git(&["format-patch", "-q", "-1", "side", "-o", patches.to_str().unwrap()]);
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

    /// Copy the seed, dirty `o` when asked, apply `config`, then run `args` under
    /// `bin` with every identity variable removed and `env` set.
    fn outcome(
        &self,
        bin: &str,
        n: usize,
        dirty: bool,
        config: &[(&str, &str)],
        env: &[(&str, &str)],
        args: &[&str],
    ) -> Outcome {
        let dir = self.root.join(format!("{}-{n}", if bin == ZVCS { "zvcs" } else { "stock" }));
        let cp = Command::new("/bin/cp").arg("-Rp").arg(self.root.join("seed")).arg(&dir).status().unwrap();
        assert!(cp.success());
        if dirty {
            std::fs::write(dir.join("o"), "o\ndirty\n").unwrap();
        }
        for (k, v) in config {
            self.ok(&dir, &["config", k, v]);
        }
        let mut c = self.cmd(bin, &dir, args);
        for k in IDENT_VARS {
            c.env_remove(k);
        }
        c.env("GIT_AUTHOR_DATE", "1700000100 +0000").env("GIT_COMMITTER_DATE", "1700000100 +0000");
        for (k, v) in env {
            c.env(k, v);
        }
        let out = c.output().unwrap();
        let root = self.root.to_string_lossy().into_owned();
        let clean = |b: &[u8]| String::from_utf8_lossy(b).replace(&root, "<root>");
        let mut state_files: Vec<String> = std::fs::read_dir(dir.join(".git"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.chars().all(|c| c.is_ascii_uppercase() || c == '_'))
            .collect();
        state_files.sort();
        Outcome {
            code: out.status.code().expect("no signal"),
            stdout: clean(&out.stdout),
            stderr: clean(&out.stderr),
            refs: self.stock_out(&dir, &["for-each-ref"]),
            status: self.stock_out(&dir, &["status", "--porcelain"]),
            log: self.stock_out(&dir, &["log", "--all", "--format=%H %an <%ae> %ad | %cn <%ce> %cd %s"]),
            state_files,
            fsck: self.stock_out(&dir, &["fsck", "--no-progress"]),
        }
    }

    #[track_caller]
    fn same(&self, dirty: bool, config: &[(&str, &str)], env: &[(&str, &str)], args: &[&str]) -> Outcome {
        let n = self.n.get();
        self.n.set(n + 1);
        let stock = self.outcome(self.stock, n, dirty, config, env, args);
        let zvcs = self.outcome(ZVCS, n, dirty, config, env, args);
        assert_eq!(zvcs, stock, "git {args:?} with {config:?} / {env:?}: left is zvcs, right is stock");
        stock
    }
}

/// A real merge over a dirty tree: the autostash is made, `prepare_to_commit()` writes
/// the merge state, and `commit_tree()`'s author lookup is what refuses a lone
/// `GIT_COMMITTER_EMAIL` — `MERGE_AUTOSTASH` and the staged result stay behind.
#[test]
fn merge_dies_at_the_merge_commits_author() {
    let Some(f) = Fixture::new("merge") else { return };
    for env in ENVS {
        f.same(true, &[], env, &["merge", "--autostash", "--no-edit", "side"]);
        f.same(false, &[], env, &["merge", "--no-ff", "-m", "x", "side"]);
        f.same(false, &[], env, &["merge", "-s", "resolve", "--no-edit", "side"]);
        f.same(false, &[], env, &["merge", "--no-commit", "side"]);
    }
    let got = f.same(true, &[], ENVS[0], &["merge", "--autostash", "--no-edit", "side"]);
    if got.code == 128 {
        // `cmd_merge()` asks for the committer strictly right after the autostash
        // (builtin/merge.c:1761-1766), so an account whose gecos field is empty —
        // a CI runner's — is refused there over the committer's defaulted name
        // (ident.c:509-514) before the merge commit's author is looked up.
        let role = if got.stderr.contains("fatal: empty ident name (for <ce@example.com>)") {
            "Committer"
        } else {
            "Author"
        };
        assert!(got.stderr.starts_with(&format!("{role} identity unknown\n")), "{}", got.stderr);
        assert!(got.state_files.iter().any(|s| s == "MERGE_AUTOSTASH"), "{:?}", got.state_files);
    }
}

/// `commit-tree`, `commit` and `notes add` give no author of their own, so each asks
/// for both; `--amend`, `-C` and `--author` name the author and leave the committer.
#[test]
fn commit_verbs_ask_for_the_author_then_the_committer() {
    let Some(f) = Fixture::new("commit") else { return };
    for env in ENVS {
        f.same(false, &[], env, &["commit-tree", "-m", "x", "-p", "HEAD", "HEAD^{tree}"]);
        f.same(false, &[], env, &["commit", "--allow-empty", "-m", "e"]);
        f.same(true, &[], env, &["commit", "-a", "-m", "e"]);
        f.same(false, &[], env, &["commit", "-m", "nothing staged"]);
        f.same(false, &[], env, &["commit", "--amend", "--no-edit"]);
        f.same(false, &[], env, &["commit", "--amend", "--reset-author", "--no-edit"]);
        f.same(false, &[], env, &["commit", "--allow-empty", "-C", "side"]);
        f.same(false, &[], env, &["commit", "--allow-empty", "-m", "e", "--author=X <x@example.com>"]);
        f.same(false, &[], env, &["notes", "add", "-m", "n"]);
    }
    // A subcommand that commits nothing never asks, even under `user.useConfigOnly`.
    let got = f.same(false, &[("user.useConfigOnly", "true")], &[], &["notes", "list"]);
    assert_eq!(got.code, 0);
}

/// The environment outranks `<role>.email` (ident.c:565-589), and a `committer.email`
/// alone leaves the author on an empty default address.
#[test]
fn environment_outranks_the_role_configuration() {
    let Some(f) = Fixture::new("precedence") else { return };
    let config = [("committer.email", "q@example.com")];
    for env in ENVS {
        f.same(false, &config, env, &["commit", "--allow-empty", "-m", "e"]);
        f.same(false, &config, env, &["merge", "--no-edit", "side"]);
    }
}

/// A pick keeps the picked author, so only the committer is looked up; a revert is
/// the user's own, so both are. Either dies with the result applied.
#[test]
fn picks_ask_for_the_committer_and_reverts_for_both() {
    let Some(f) = Fixture::new("pick") else { return };
    for env in ENVS {
        f.same(false, &[], env, &["cherry-pick", "side"]);
        f.same(false, &[], env, &["cherry-pick", "-x", "side"]);
        f.same(false, &[], env, &["revert", "--no-edit", "HEAD"]);
        f.same(false, &[], env, &["cherry-pick", "--no-commit", "side"]);
    }
}

/// `print_commit_summary()` adds ` Committer:` and the implicit-identity advice when
/// the committer's address was not given (sequencer.c:1453-1460). An `author.email`
/// alone is a `*.email` key, so `ident_default_email()` computes nothing and the
/// committer commits on an empty address the summary names — on any machine,
/// whatever its host or gecos field. The advice reads `~/.gitconfig` existing as
/// the `config` wording (sequencer.c:1399-1413) and `advice.implicitIdentity` turns
/// it off; `GIT_COMMITTER_EMAIL`, even empty, or `user.email` makes it sufficient.
#[test]
fn the_summary_names_a_committer_whose_address_was_not_given() {
    let Some(f) = Fixture::new("summary") else { return };
    let config: &[(&str, &str)] = &[("author.email", "ae@example.com")];
    let quiet: &[(&str, &str)] = &[("author.email", "ae@example.com"), ("advice.implicitIdentity", "false")];
    let user: &[(&str, &str)] = &[("user.email", "ue@example.com")];
    let envs: &[&[(&str, &str)]] = &[
        &[("GIT_COMMITTER_NAME", "Env C")],
        &[("GIT_COMMITTER_NAME", "Env C"), ("GIT_COMMITTER_EMAIL", "")],
        &[("GIT_COMMITTER_NAME", "Env C"), ("EMAIL", "e@example.com")],
    ];
    for home_config in [false, true] {
        if home_config {
            std::fs::write(f.root.join(".gitconfig"), "").unwrap();
        }
        for config in [config, quiet, user] {
            for env in envs {
                f.same(false, config, env, &["commit", "--allow-empty", "-m", "e"]);
                f.same(false, config, env, &["commit", "--amend", "--no-edit"]);
                f.same(false, config, env, &["cherry-pick", "side"]);
                f.same(false, config, env, &["revert", "--no-edit", "HEAD"]);
            }
        }
    }
    let got = f.same(false, config, envs[0], &["commit", "--allow-empty", "-m", "e"]);
    assert!(got.stdout.contains("\n Committer: Env C <>\n"), "{}", got.stdout);
}

/// A merge-backend rebase replays each pick with its own author, so only the
/// committer is looked up, and the death keeps the state directory resumable.
#[test]
fn rebase_picks_ask_for_the_committer() {
    let Some(f) = Fixture::new("rebase") else { return };
    for env in ENVS {
        f.same(false, &[], env, &["rebase", "side"]);
    }
}

/// `git am` checks the committer before it reads the index or touches a session
/// (builtin/am.c:2464-2465); the commits take the mail's author.
#[test]
fn am_checks_the_committer_up_front() {
    let Some(f) = Fixture::new("am") else { return };
    let patch = f.root.join("patches").join("0001-side.patch");
    let patch = patch.to_str().unwrap();
    for env in ENVS {
        f.same(false, &[], env, &["am", patch]);
        f.same(false, &[], env, &["am", "-3", patch]);
    }
}
