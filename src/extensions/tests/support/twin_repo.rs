//! Two identical repositories, one driven by stock git and one by zvcs, with the
//! small history the sequencer and merge parity tests share.
//!
//! `main` holds `a` at `1`, `1 2`, `1 2 3` (commits `one`, `two`, `three`); `side` forks
//! from `one` and appends `2` itself (`two-again`), so
//!
//! * `cherry-pick side`, `merge side` and `rebase side` conflict on `a`, and
//! * `side^` is a fork of `main` that already contains `two`'s change, so picking `main~1`
//!   onto `side` is an empty pick.
//!
//! Included with
//!
//! ```ignore
//! #[path = "support/stock_git.rs"]
//! mod stock_git;
//! #[path = "support/twin_repo.rs"]
//! mod twin_repo;
//! ```

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

pub const ZVCS: &str = env!("CARGO_BIN_EXE_git");

/// What one run answered: exit status and both streams, root-normalised.
#[derive(Debug, PartialEq, Eq)]
pub struct Out {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// One world: a directory holding the repository at `root/repo`, driven by `bin`.
pub struct Side {
    pub root: PathBuf,
    pub bin: String,
}

impl Drop for Side {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Side {
    pub fn repo(&self) -> PathBuf {
        self.root.join("repo")
    }

    /// Run `git <args>` in `dir` with a clean environment plus `env`.
    pub fn run_in(&self, dir: &Path, env: &[(&str, &str)], args: &[&str]) -> Out {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args)
            .current_dir(dir)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.root)
            .env("GIT_CEILING_DIRECTORIES", self.root.parent().unwrap())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_EDITOR", "true")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C");
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        let root = self.root.to_string_lossy().into_owned();
        let text = |b: &[u8]| String::from_utf8_lossy(b).replace(&root, "<root>");
        Out { code: out.status.code().unwrap_or(-1), stdout: text(&out.stdout), stderr: text(&out.stderr) }
    }

    pub fn git(&self, args: &[&str]) -> Out {
        self.run_in(&self.repo(), &[], args)
    }

    pub fn git_env(&self, env: &[(&str, &str)], args: &[&str]) -> Out {
        self.run_in(&self.repo(), env, args)
    }

    pub fn write(&self, rel: &str, body: &str) {
        let path = self.repo().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    pub fn read(&self, rel: &str) -> Option<Vec<u8>> {
        std::fs::read(self.repo().join(rel)).ok()
    }

    fn history(&self) {
        let init = self.run_in(&self.root, &[], &["init", "-q", "-b", "main", "repo"]);
        assert_eq!(init.code, 0, "{init:?}");
        let mut body = String::new();
        for (n, name) in [(1, "one"), (2, "two"), (3, "three")] {
            body.push_str(&format!("{n}\n"));
            self.write("a", &body);
            self.git(&["add", "a"]);
            let commit = self.git(&["commit", "-q", "-m", name]);
            assert_eq!(commit.code, 0, "{commit:?}");
        }
        self.git(&["checkout", "-q", "-b", "side", "main~2"]);
        self.write("a", "1\n2\n");
        self.git(&["commit", "-q", "-am", "two-again"]);
        self.git(&["checkout", "-q", "main"]);
    }
}

/// Build the stock and zvcs worlds for `label`, each holding the same history.
pub fn pair(label: &str, stock: &str) -> (Side, Side) {
    let base = std::env::temp_dir().join(format!("zvcs-twin-repo-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let mut sides = [(stock, "stock"), (ZVCS, "zvcs")].map(|(bin, name)| {
        let root = base.join(name);
        std::fs::create_dir_all(&root).unwrap();
        let side = Side { root: std::fs::canonicalize(&root).unwrap(), bin: bin.to_owned() };
        side.history();
        side
    })
    .into_iter();
    (sides.next().unwrap(), sides.next().unwrap())
}

/// The index extension named `sig` (`TREE`, `EOIE`, …) as raw bytes, found by walking the
/// extension table that follows the entries; `None` when the index carries none.
///
/// Entries are not decoded: the extensions are located from the back, since every extension
/// is `signature(4) + size(4) + data` and the file ends in a 20-byte checksum.
pub fn index_extension(index: &[u8], sig: &[u8; 4]) -> Option<Vec<u8>> {
    // The extension table starts after the last entry; scanning candidates from the first
    // signature-shaped position onwards is enough for the small fixtures these tests use.
    let body = &index[..index.len().saturating_sub(20)];
    (12..body.len().saturating_sub(8)).find_map(|at| {
        if &body[at..at + 4] != sig {
            return None;
        }
        let size = u32::from_be_bytes(body[at + 4..at + 8].try_into().ok()?) as usize;
        let end = at + 8 + size;
        (end <= body.len() && body[at + 8..end].len() == size).then(|| body[at + 8..end].to_vec())
    })
}
