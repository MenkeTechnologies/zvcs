//! A pair of identical worlds, one driven by stock git and one by zvcs, so a
//! test states a command once and asserts the two answer alike.
//!
//! Each world is `root/up` (a repository with `main` at `b`, tagged `v0.1.0` on
//! `a`) and `root/work` (a clone of `up` taken one commit earlier, so a fetch
//! has something to bring in). Output is compared after the world's own root is
//! replaced by `<root>`, which is the only part that legitimately differs.
//!
//! Included with
//!
//! ```ignore
//! #[path = "support/stock_git.rs"]
//! mod stock_git;
//! #[path = "support/twin.rs"]
//! mod twin;
//! ```

#![allow(dead_code)]

use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::stock_git::stock_git;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

/// What one run left behind: exit status and both streams, root-normalised.
#[derive(Debug, PartialEq, Eq)]
pub struct Outcome {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub struct World {
    root: PathBuf,
    bin: String,
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl World {
    fn build(label: &str, bin: &str) -> World {
        let root = std::env::temp_dir().join(format!("zvcs-twin-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // Resolve symlinks (`/var` -> `/private/var` on macOS) so a path git prints
        // from `getcwd()` still starts with the root we strip.
        let root = std::fs::canonicalize(&root).unwrap();
        let w = World { root, bin: bin.to_owned() };
        let up = w.root.join("up");
        w.git(&w.root, &["init", "-q", "-b", "main", "up"]);
        w.git(&up, &["commit", "-q", "--allow-empty", "-m", "a"]);
        w.git(&up, &["tag", "v0.1.0"]);
        w.git(&w.root, &["clone", "-q", "up", "work"]);
        w.git(&up, &["commit", "-q", "--allow-empty", "-m", "b"]);
        w
    }

    fn git(&self, dir: &Path, args: &[&str]) -> Outcome {
        self.git_env(dir, args, &[])
    }

    fn git_env(&self, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Outcome {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args)
            .current_dir(dir)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.root)
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
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
        let clean = |bytes: &[u8]| String::from_utf8_lossy(bytes).replace(&root, "<root>");
        Outcome {
            // A death by signal reports as the shell would: 128 + the signal number.
            code: out.status.code().unwrap_or_else(|| 128 + out.status.signal().expect("no exit code and no signal")),
            stdout: clean(&out.stdout),
            stderr: clean(&out.stderr),
        }
    }
}

/// The two worlds for one test, or `None` when this machine has no stock git.
pub struct Twin {
    stock: World,
    zvcs: World,
}

impl Twin {
    pub fn new(label: &str) -> Option<Twin> {
        let stock = stock_git()?;
        Some(Twin {
            stock: World::build(&format!("{label}-stock"), stock),
            zvcs: World::build(&format!("{label}-zvcs"), ZVCS),
        })
    }

    /// Run `args` in `<root>/<subdir>` of both worlds and return `(stock, zvcs)`.
    pub fn run_in(&self, subdir: &str, args: &[&str]) -> (Outcome, Outcome) {
        self.run_env_in(subdir, args, &[])
    }

    pub fn run_env_in(&self, subdir: &str, args: &[&str], env: &[(&str, &str)]) -> (Outcome, Outcome) {
        (
            self.stock.git_env(&self.stock.root.join(subdir), args, env),
            self.zvcs.git_env(&self.zvcs.root.join(subdir), args, env),
        )
    }

    /// Run `args` in `work` and assert the two agree on exit, stdout and stderr.
    #[track_caller]
    pub fn same(&self, args: &[&str]) {
        self.same_in("work", args);
    }

    #[track_caller]
    pub fn same_in(&self, subdir: &str, args: &[&str]) {
        let (stock, zvcs) = self.run_in(subdir, args);
        assert_eq!(zvcs, stock, "git {args:?} in {subdir}: left is zvcs, right is stock");
    }

    /// Run setup `args` in `work` of both worlds without comparing them.
    pub fn prepare(&self, args: &[&str]) {
        self.run_in("work", args);
    }

    /// Remove the file `rel` (relative to the world root) from both worlds, if present.
    pub fn forget(&self, rel: &str) {
        for w in [&self.stock, &self.zvcs] {
            let _ = std::fs::remove_file(w.root.join(rel));
        }
    }

    /// Create the directory `rel` (relative to the world root) in both worlds.
    pub fn mkdir(&self, rel: &str) {
        for w in [&self.stock, &self.zvcs] {
            std::fs::create_dir_all(w.root.join(rel)).unwrap();
        }
    }

    /// The contents of `path` (relative to the world root) in both worlds.
    pub fn read(&self, path: &str) -> (Option<String>, Option<String>) {
        let read = |w: &World| std::fs::read_to_string(w.root.join(path)).ok();
        (read(&self.stock), read(&self.zvcs))
    }
}
