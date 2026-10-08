//! Two byte-identical copies of one fixture, one driven by stock git and one by zvcs, so a
//! test states a command once with a configuration value in place and asserts the two
//! answer alike.
//!
//! The fixture is `main` at `two` (parent `one`, tagged `v1`) with a branch `side` at
//! `one`. The value is written into `.git/config` directly because `git config` itself
//! refuses to run once the value is a bad one. Output is compared after the copy's own
//! path is replaced by `<repo>`.
//!
//! Included with
//!
//! ```ignore
//! #[path = "support/stock_git.rs"]
//! mod stock_git;
//! #[path = "support/config_twins.rs"]
//! mod config_twins;
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

pub const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Everything a run is compared on.
#[derive(Debug, PartialEq, Eq)]
pub struct Outcome {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

pub fn run(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> Outcome {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("GIT_PAGER", "cat")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .unwrap();
    let shown = dir.to_string_lossy().into_owned();
    let scrub = |bytes: &[u8]| String::from_utf8_lossy(bytes).replace(&shown, "<repo>");
    Outcome {
        stdout: scrub(&out.stdout),
        stderr: scrub(&out.stderr),
        code: out.status.code().unwrap_or(-1),
    }
}

/// Two copies of one fixture: stock git works in `stock`, zvcs in `zvcs`.
pub struct Twins {
    pub root: PathBuf,
    pub stock_bin: &'static str,
}

impl Drop for Twins {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Twins {
    pub fn new(tag: &str) -> Option<Twins> {
        let stock_bin = super::stock_git::stock_git()?;
        let root = std::env::temp_dir().join(format!("zvcs-cfg-gate-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(root.join("home")).unwrap();
        let t = Twins { root: root.canonicalize().unwrap(), stock_bin };
        let (src, home) = (t.root.join("src"), t.root.join("home"));
        let git = |args: &[&str]| {
            let o = run(stock_bin, &src, &home, args);
            assert_eq!(o.code, 0, "fixture `git {args:?}`: {}", o.stderr);
        };
        git(&["init", "-q", "-b", "main", "."]);
        // No background housekeeping may touch the tree while it is being copied.
        git(&["config", "gc.auto", "0"]);
        git(&["config", "maintenance.auto", "false"]);
        std::fs::write(src.join("file"), "one\n").unwrap();
        git(&["add", "file"]);
        git(&["commit", "-q", "-m", "one"]);
        git(&["tag", "v1"]);
        git(&["branch", "side"]);
        std::fs::write(src.join("file"), "two\n").unwrap();
        git(&["commit", "-q", "-am", "two"]);
        for side in ["stock", "zvcs"] {
            let status = Command::new("cp")
                .arg("-R")
                .arg(&src)
                .arg(t.root.join(side))
                .status()
                .unwrap();
            assert!(status.success());
        }
        Some(t)
    }

    /// Replace the repository config of both sides with the fixture's own plus one
    /// `section.name = value` line per entry of `entries`. The file is written
    /// directly: `git config` itself refuses to run once the value is a bad one.
    pub fn set_config(&self, entries: &[(&str, &str)]) {
        for side in ["stock", "zvcs"] {
            let path = self.root.join(side).join(".git/config");
            let base = self.root.join("base-config");
            if !base.exists() {
                std::fs::copy(&path, &base).unwrap();
            }
            let mut text = std::fs::read_to_string(&base).unwrap();
            for (key, value) in entries {
                let (section, name) = key.split_once('.').expect("section.name");
                text.push_str(&format!("[{section}]\n\t{name} = {value}\n"));
            }
            std::fs::write(path, text).unwrap();
        }
    }

    /// Run `args` on both sides with `key=value` in the repository's own config,
    /// assert the outcomes are equal, and return the stock one.
    pub fn same_with(&self, key: &str, value: &str, args: &[&str]) -> (Outcome, Outcome) {
        let home = self.root.join("home");
        self.set_config(&[(key, value)]);
        let stock = run(self.stock_bin, &self.root.join("stock"), &home, args);
        let zvcs = run(BIN, &self.root.join("zvcs"), &home, args);
        assert_eq!(
            stock, zvcs,
            "`git {args:?}` with {key}={value}: stock (left) vs zvcs (right)"
        );
        (stock, zvcs)
    }

    /// The ref snapshot of one side, to show a refused run changed nothing.
    pub fn refs(&self, side: &str) -> String {
        run(self.stock_bin, &self.root.join(side), &self.root.join("home"), &["show-ref"]).stdout
    }
}

