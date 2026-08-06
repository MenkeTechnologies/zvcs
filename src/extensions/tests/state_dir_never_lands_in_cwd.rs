//! The state directory is never the directory the caller happened to be in.
//!
//! `zvcs_home()` resolves `ZVCS_HOME`, then `$HOME/.zvcs`. With neither set the
//! state still has to go somewhere, and the earlier fallback was a *relative*
//! `.zvcs` — so any invocation with a cleared environment deposited a state
//! directory into its working directory, including invocations that only ask a
//! question. Cargo runs each test binary from its crate root, and the probe that
//! asks a binary whether it is zvcs clears the environment deliberately, so the
//! two together left `src/extensions/.zvcs` and `src/parity/.zvcs` in the source
//! tree.
//!
//! A tool that writes into whatever directory it is run from is a tool that
//! cannot be run from a directory you care about, so this is checked directly:
//! run the binary in an empty directory with no environment at all, and require
//! the directory to still be empty.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// An empty directory that removes itself.
struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Dir {
    fn new(tag: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("zvcs-cwdstate-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Dir(path)
    }

    /// What the directory holds, `.` and `..` excluded.
    fn entries(&self) -> Vec<String> {
        let mut out: Vec<String> = std::fs::read_dir(&self.0)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        out.sort();
        out
    }
}

/// With no environment at all, nothing is written to the working directory.
#[test]
fn a_cleared_environment_writes_nothing_to_the_working_directory() {
    // `zverbs` is the probe that finds out whether a binary is zvcs, and clearing
    // the environment is what makes it sound — zvcs's own installation puts a
    // `git-zverbs` shim on `PATH`, which a stock git would otherwise answer too.
    // It is also the cheapest possible read: if this leaves something behind,
    // everything does.
    for args in [vec!["zverbs"], vec!["--version"], vec!["rev-parse", "--git-dir"]] {
        let dir = Dir::new("cleared");
        let out = Command::new(BIN)
            .args(&args)
            .current_dir(&dir.0)
            .env_clear()
            .output()
            .expect("run binary");
        assert_eq!(
            dir.entries(),
            Vec::<String>::new(),
            "`git {args:?}` left {:?} behind (exit {:?})",
            dir.entries(),
            out.status.code()
        );
    }
}

/// `ZVCS_HOME` is honoured, and it is the only place the state goes.
#[test]
fn the_state_directory_follows_zvcs_home() {
    let dir = Dir::new("home");
    let home = dir.0.join("state");

    let out = Command::new(BIN)
        .arg("zverbs")
        .current_dir(&dir.0)
        .env_clear()
        .env("ZVCS_HOME", &home)
        .output()
        .expect("run binary");
    assert!(out.status.success(), "{out:?}");

    // The named directory is the one that gets created; the working directory is
    // left as it was, `state` aside.
    assert!(home.is_dir(), "ZVCS_HOME was not created");
    assert_eq!(dir.entries(), ["state"]);
}

/// With `HOME` set and `ZVCS_HOME` unset, the state is `$HOME/.zvcs` — not a
/// `.zvcs` beside whatever the caller was standing in.
#[test]
fn a_home_without_zvcs_home_uses_the_dot_directory_under_it() {
    let dir = Dir::new("dothome");
    let home = dir.0.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let work = dir.0.join("work");
    std::fs::create_dir_all(&work).unwrap();

    let out = Command::new(BIN)
        .arg("zverbs")
        .current_dir(&work)
        .env_clear()
        .env("HOME", &home)
        .output()
        .expect("run binary");
    assert!(out.status.success(), "{out:?}");

    assert!(home.join(".zvcs").is_dir(), "$HOME/.zvcs was not created");
    let left: Vec<String> = std::fs::read_dir(&work)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(left.is_empty(), "the working directory got {left:?}");
}
