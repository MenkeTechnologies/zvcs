//! Which file `git config` operates on, and what it refuses when that "file" is not one.
//!
//! `location_options_init()` (builtin/config.c:929-1005) resolves the scope before any
//! action runs, and three of its decisions were missing from the port:
//!
//!   * `GIT_CONFIG` names the single file when no scope flag was given
//!     (builtin/config.c:932-934), and is counted alongside `--file`, so it collides with
//!     a scope flag that was given — `error(_("only one config file at a time"))`, 129.
//!   * `--file -` is standard input, not a file called `-` (builtin/config.c:954-958):
//!     its entries carry `CONFIG_ORIGIN_STDIN`, whose `--show-origin` column is the bare
//!     `standard input:` (config.c:3601), its parse errors say `in standard input`
//!     (config.c:1151), and it follows includes by default because `source.file` is left
//!     NULL. Writing it is `die(_("writing to stdin is not supported"))` (:817) and
//!     editing it `die(_("editing stdin is not supported"))` (:1297).
//!   * `--local`, `--worktree` and `--blob` outside a repository are `die()`s
//!     (builtin/config.c:941-948) — exit 128 — and they fire for reads too, so they are
//!     never confused with "key not found" (1).
//!
//! Every expectation was measured from stock git 2.55.0 under the same environment.
#![cfg(unix)]

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// A bare directory — no repository — so the repo-required scopes can be exercised.
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("zvcs-cfg-loc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Fixture { root }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.arg("config")
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env_remove("GIT_CONFIG")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .env("LC_ALL", "C")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        c
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = self.cmd(args).output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    /// The same, with `input` on the command's standard input.
    fn run_stdin(&self, args: &[&str], input: &str) -> (String, String, i32) {
        let mut child = self
            .cmd(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

/// `--file -` reads the config from standard input.
#[test]
fn file_dash_reads_standard_input() {
    let f = Fixture::new("read");
    let (out, err, code) = f.run_stdin(&["--list", "--file", "-"], "[ein]\n\tbahn = strasse\n");
    assert_eq!((out.as_str(), err.as_str(), code), ("ein.bahn=strasse\n", "", 0));

    let (out, _, code) =
        f.run_stdin(&["--file", "-", "ein.bahn"], "[ein]\n\tbahn = strasse\n");
    assert_eq!((out.as_str(), code), ("strasse\n", 0));
}

/// The `--show-origin` column for a stdin entry is the bare `standard input:` — there is
/// no filename to name after the colon.
#[test]
fn stdin_entries_show_standard_input_as_their_origin() {
    let f = Fixture::new("origin");
    let (out, _, code) =
        f.run_stdin(&["--list", "--file", "-", "--show-origin"], "[user]\n\tcustom = true\n");
    assert_eq!((out.as_str(), code), ("standard input:\tuser.custom=true\n", 0));
}

/// An `include.path` followed out of stdin lands in a real file, so *that* entry's origin
/// is the file — and includes are followed from stdin without `--includes`, because
/// `source.file` is NULL for it.
#[test]
fn an_include_out_of_stdin_keeps_the_included_files_origin() {
    let f = Fixture::new("inc");
    let included = f.root.join("stdin.include");
    std::fs::write(&included, "[user]\n\tstdin = include\n").unwrap();
    let stdin = format!("[include]path=\"{}\"\n", included.display());

    let (out, _, code) =
        f.run_stdin(&["--show-origin", "--includes", "--file", "-", "user.stdin"], &stdin);
    assert_eq!((out.as_str(), code), (format!("file:{}\tinclude\n", included.display()).as_str(), 0));

    let (out, _, code) = f.run_stdin(&["--file", "-", "user.stdin"], &stdin);
    assert_eq!((out.as_str(), code), ("include\n", 0));
}

/// A stdin config that does not parse names `standard input`, not a file.
#[test]
fn a_malformed_stdin_config_names_standard_input() {
    let f = Fixture::new("bad");
    let (_, err, code) = f.run_stdin(&["--list", "--file", "-"], "[broken\n");
    assert!(err.contains("bad config line 1 in standard input"), "{err}");
    assert_ne!(code, 0);
}

/// Standard input cannot be written to or edited.
#[test]
fn writing_or_editing_stdin_is_fatal() {
    let f = Fixture::new("write");

    let (out, err, code) = f.run_stdin(&["--file", "-", "foo.bar", "baz"], "");
    assert_eq!((out.as_str(), err.as_str(), code), ("", "fatal: writing to stdin is not supported\n", 128));

    let (out, err, code) = f.run_stdin(&["--edit", "--file", "-"], "");
    assert_eq!((out.as_str(), err.as_str(), code), ("", "fatal: editing stdin is not supported\n", 128));
}

/// `GIT_CONFIG` names the one file a scope-less `git config` reads.
#[test]
fn git_config_env_names_the_file() {
    let f = Fixture::new("env");
    let other = f.root.join("other-config");
    std::fs::write(&other, "[ein]\n\tbahn = strasse\n").unwrap();

    let out = f
        .cmd(&["--list"])
        .env("GIT_CONFIG", &other)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ein.bahn=strasse\n");
    assert_eq!(out.status.code(), Some(0));

    // It is counted like `--file`, so a scope flag alongside it is a usage error.
    let out = f
        .cmd(&["--list", "--global"])
        .env("GIT_CONFIG", &other)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(129));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("only one config file at a time"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The repo-required scopes die with 128 rather than looking like "key not found" (1).
#[test]
fn repo_scopes_outside_a_repository_are_fatal() {
    let f = Fixture::new("norepo");

    for (flag, word) in [("--local", "--local"), ("--worktree", "--worktree")] {
        let (out, err, code) = f.run(&[flag, "foo.bar"]);
        assert_eq!((out.as_str(), code), ("", 128), "{flag}");
        assert_eq!(err, format!("fatal: {word} can only be used inside a git repository\n"));
    }

    let (out, err, code) = f.run(&["--blob", "HEAD:cfg", "foo.bar"]);
    assert_eq!((out.as_str(), code), ("", 128));
    assert_eq!(err, "fatal: --blob can only be used inside a git repository\n");
}
