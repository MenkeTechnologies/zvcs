//! What git's configuration reader has delivered when it dies, and the include
//! loop it dies on.
//!
//! `git_parse_source()` hands every entry to the callback as it completes and
//! dies with `bad config line <n> in file <f>` where it stops (config.c:1055-1150),
//! so `config --file <f> --list` has already written the entries ahead of the
//! bad line. `git_config_include()` reads an `include.path` where it stands and
//! `handle_path_include()` dies with `include_depth_advice` past
//! `MAX_INCLUDE_DEPTH` (config.c:135-191, :419-440), after the include lines of
//! every level were listed. The same reader runs at startup over the system,
//! global and repository files, so a circular include there ends every command,
//! `version` and `status` included, before it runs.
//!
//! Each case runs stock git and zvcs on the same files and compares stdout,
//! stderr and the exit code byte for byte.

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch(tag: &str) -> Scratch {
    let root = std::env::temp_dir().join(format!("zvcs-config-stream-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    Scratch(std::fs::canonicalize(&root).unwrap())
}

fn run(bin: &str, cwd: &Path, home: &Path, global: &str, args: &[&str]) -> (Vec<u8>, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", global)
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    (out.stdout, String::from_utf8_lossy(&out.stderr).into_owned(), out.status.code().expect("no signal"))
}

/// Run `args` through both binaries and require the same answer; returns it.
fn same(cwd: &Path, home: &Path, global: &str, args: &[&str]) -> Option<(Vec<u8>, String, i32)> {
    let stock = stock_git()?;
    let want = run(stock, cwd, home, global, args);
    let got = run(ZVCS, cwd, home, global, args);
    assert_eq!(
        (String::from_utf8_lossy(&got.0), &got.1, got.2),
        (String::from_utf8_lossy(&want.0), &want.1, want.2),
        "zvcs vs stock for {args:?} in {}",
        cwd.display()
    );
    Some(want)
}

#[test]
fn file_list_streams_the_entries_ahead_of_a_bad_line() {
    let s = scratch("badline");
    let d = &s.0;
    let files: &[(&str, &str)] = &[
        ("header", "[a]\n\tx = 1\n\ty = 2\n[b\n\tz = 3\n"),
        ("quote", "[a]\n\tx = 1\n\ty = \"abc\n\tz = 2\n"),
        ("escape", "[a]\n\tx = 1\n\ty = a\\q\n"),
        ("key", "[a]\n\tx = 1\n\tbad key = 2\n"),
        ("comment", "[a]\n\tx = 1\nb;c\n"),
        ("continued", "[a]\n\tx = 1\n\ty = one \\\n two\n\tw x\n"),
        ("valueless", "[a]\n\tx = 1\n\ty = 2 ; c\n\tz\n\t= 3\n"),
        ("subsection", "[a]\n\tx = 1\n[b \"Sub\"]\n\tk = v\n[c\n"),
    ];
    for (name, body) in files {
        std::fs::write(d.join(name), body).unwrap();
    }
    for (name, _) in files {
        for extra in [&["--list"][..], &["--list", "--show-origin"], &["-l", "-z"], &["--list", "--name-only"]] {
            let mut args = vec!["config", "-f", name];
            args.extend_from_slice(extra);
            let Some((_, err, code)) = same(d, d, "/dev/null", &args) else { return };
            assert_eq!(code, 128, "{args:?}: {err}");
        }
        // The get forms collect their matches first, so nothing reaches stdout.
        let Some((out, _, code)) = same(d, d, "/dev/null", &["config", "-f", name, "--get-regexp", "."]) else {
            return;
        };
        assert!(out.is_empty() && code == 128);
    }
    let (out, _, _) = same(d, d, "/dev/null", &["config", "-f", "header", "--list"]).unwrap();
    assert_eq!(out, b"a.x=1\na.y=2\n");
}

#[test]
fn file_include_loop_lists_every_level_then_dies() {
    let s = scratch("fileloop");
    let d = &s.0;
    std::fs::write(d.join("self"), "[include]\n\tpath = self\n[a]\n\tx = 1\n").unwrap();
    std::fs::write(d.join("one"), "[a]\n\tx = 1\n[include]\n\tpath = two\n[z]\n\tq = 9\n").unwrap();
    std::fs::write(d.join("two"), "[b]\n\ty = 2\n[include]\n\tpath = one\n").unwrap();
    std::fs::create_dir_all(d.join("sub")).unwrap();
    // A missing include is skipped, a relative one is resolved against the
    // including file's directory, and the chain still closes on itself.
    std::fs::write(d.join("three"), "[include]\n\tpath = nothere\n\tpath = \"sub/four\"\n[t]\n\tv = 1\n").unwrap();
    std::fs::write(d.join("sub/four"), "[s]\n\tw = 1\n[include]\n\tpath = ../three\n").unwrap();
    for file in ["self", "one", "three", "sub/four"] {
        for extra in [&["--list"][..], &["--list", "--show-origin"], &["-l", "-z", "--show-scope"]] {
            let mut args = vec!["config", "-f", file, "--includes"];
            args.extend_from_slice(extra);
            let Some((_, err, code)) = same(d, d, "/dev/null", &args) else { return };
            assert_eq!(code, 128);
            assert!(err.starts_with("fatal: exceeded maximum include depth (10) while including\n"), "{err}");
        }
        let Some((out, _, _)) = same(d, d, "/dev/null", &["config", "-f", file, "--includes", "--get-all", "a.x"]) else {
            return;
        };
        assert!(out.is_empty());
        // Without --includes a --file is read alone.
        same(d, d, "/dev/null", &["config", "-f", file, "--list"]);
    }
    let (out, _, _) = same(d, d, "/dev/null", &["config", "-f", "self", "--includes", "--list"]).unwrap();
    assert_eq!(out, "include.path=self\n".repeat(11).into_bytes());
}

#[test]
fn startup_include_loop_ends_every_command() {
    let s = scratch("startup");
    let d = &s.0;
    let Some(stock) = stock_git() else { return };
    let repo = d.join("repo");
    let init = run(stock, d, d, "/dev/null", &["init", "-q", "repo"]);
    assert_eq!(init.2, 0, "{}", init.1);
    std::fs::create_dir_all(repo.join("x/y")).unwrap();
    let local = repo.join(".git/config");
    let mut text = std::fs::read_to_string(&local).unwrap();
    text.push_str("[include]\n\tpath = config\n");
    std::fs::write(&local, text).unwrap();
    let nested = repo.join("x/y");
    for args in [
        &["status"][..],
        &["rev-parse", "HEAD"],
        &["config", "--list"],
        &["config", "get", "core.bare"],
        &["config", "--global", "--list"],
        &["config", "--local", "--list"],
    ] {
        for cwd in [&repo, &nested] {
            let (out, err, code) = same(cwd, d, "/dev/null", args).unwrap();
            assert!(out.is_empty() && code == 128, "{args:?}: {err}");
        }
    }

    // The global file is read ahead of the repository's, and by every command.
    let global = d.join("gloop");
    std::fs::write(&global, "[a]\n\tx = 1\n[include]\n\tpath = gloop\n").unwrap();
    let global = global.to_str().unwrap();
    for args in [&["version"][..], &["config", "--global", "--list"], &["config", "get", "a.x"]] {
        let (_, err, code) = same(&repo, d, global, args).unwrap();
        assert_eq!(code, 128);
        assert!(err.contains("gloop\nfrom\n"), "{err}");
    }
}
