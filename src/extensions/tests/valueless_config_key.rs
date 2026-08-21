//! A key with no `=` is boolean *true*, and that is not the same as "readable by
//! anything".
//!
//! `git_parse_maybe_bool_text()` (parse.c:166-180) starts with
//!
//! ```c
//! if (!value)
//!         return 1;
//! if (!*value)
//!         return 0;
//! ```
//!
//! so `[core]\n\tbare\n` is true while `bare =` is false. A reader that wants a
//! string or an int gets no such courtesy: `config_error_nonbool()`
//! (config.c:3571-3579) is `return error(_("missing value for '%s'"), var)`, a
//! negative return, and `git_die_config_linenr()` (config.c:2554-2562) turns that
//! into the second line:
//!
//! ```text
//! error: missing value for 'core.abbrev'
//! fatal: bad config variable 'core.abbrev' in file '<path>' at line 2
//! ```
//!
//! `-c` cannot spell this — a command-line override is always `key` or
//! `key=value`, and the bool-only `-c key` form is command-line config with no
//! file behind it, which `git_die_config_linenr()` reports as `unable to parse
//! '<key>' from command-line config` instead. So the whole shape is file-only.
//!
//! It diverged in **both** directions, and one direction was a state divergence:
//!
//!   * `git init-db` re-initialised the repository that git refuses to touch.
//!   * `git prune-packed` refused a prune git performs, because the port ran
//!     `git_default_config()` for it and git does not — `cmd_prune_packed()`
//!     contains no config call, and its `RUN_SETUP` only reads the repository
//!     format and the index.
//!
//! Both post-states are asserted below, not just the exit codes.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn scratch(tag: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let root = std::env::temp_dir().join(format!("zvcs-novalue-{tag}-{}-{unique}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home")).expect("mkdir fixture");
    root.canonicalize().expect("canonicalize fixture")
}

/// Run with the system scope aimed at `system`, or at nothing when it is `None`.
fn run(dir: &Path, home: &Path, system: Option<&Path>, args: &[&str]) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("XDG_CONFIG_HOME");
    match system {
        Some(path) => {
            cmd.env("GIT_CONFIG_NOSYSTEM", "0");
            cmd.env("GIT_CONFIG_SYSTEM", path);
        }
        None => {
            cmd.env("GIT_CONFIG_NOSYSTEM", "1");
        }
    }
    cmd.output().expect("run zvcs git")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn ok(dir: &Path, home: &Path, args: &[&str]) -> Output {
    let out = run(dir, home, None, args);
    assert!(out.status.success(), "setup `git {args:?}`: {}", stderr(&out));
    out
}

/// How many loose object files the repository holds.
fn loose_objects(work: &Path) -> usize {
    fn walk(dir: &Path, count: &mut usize) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            if path.is_dir() {
                // Only the two-hex-digit fan-out directories hold loose objects.
                if name.len() == 2 && name.to_string_lossy().chars().all(|c| c.is_ascii_hexdigit()) {
                    walk(&path, count);
                }
            } else {
                *count += 1;
            }
        }
    }
    let mut count = 0;
    walk(&work.join(".git/objects"), &mut count);
    count
}

/// A system-scope config file holding one valueless key.
fn system_config(root: &Path, key: &str) -> PathBuf {
    let path = root.join("system-config");
    let (section, name) = key.split_once('.').expect("dotted key");
    std::fs::write(&path, format!("[{section}]\n\t{name}\n")).expect("write system config");
    path
}

/// The two-line refusal, with the file and line the value sits on.
fn nonbool_refusal(system: &Path, key: &str) -> String {
    format!(
        "error: missing value for '{key}'\nfatal: bad config variable '{key}' in file '{}' at line 2\n",
        system.display()
    )
}

/// A repository with three blobs, packed with `repack -a` so both the pack and
/// the loose copies exist — which is exactly what `prune-packed` is for.
fn packed_repo(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = scratch(tag);
    let (home, work) = (root.join("home"), root.join("wk"));
    std::fs::create_dir_all(&work).expect("mkdir work");
    ok(&work, &home, &["init", "-q", "-b", "main"]);
    for i in 1..=3 {
        std::fs::write(work.join(format!("f{i}.txt")), format!("content {i}\n")).expect("write");
    }
    ok(&work, &home, &["add", "-A"]);
    ok(&work, &home, &["commit", "-q", "-m", "one"]);
    // `-a` without `-d`: everything is packed and the loose copies stay behind.
    ok(&work, &home, &["repack", "-a", "-q"]);
    assert!(loose_objects(&work) > 0, "the fixture needs loose objects to prune");
    (root, work, home)
}

/// `prune-packed` prunes. `cmd_prune_packed()` never calls `git_config()`, so the
/// valueless `core.abbrev` that kills `init-db` is invisible to it — and the port
/// used to refuse the command, leaving every loose object in place.
#[test]
fn prune_packed_prunes_under_a_valueless_int_key() {
    let (root, work, home) = packed_repo("prunepacked");
    let system = system_config(&root, "core.abbrev");
    let before = loose_objects(&work);

    let out = run(&work, &home, Some(&system), &["prune-packed"]);

    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stderr(&out), "");
    assert_eq!(loose_objects(&work), 0, "every packed object was pruned ({before} before)");
}

/// The other two verbs on the same footing: `prune` and `mktree` are `RUN_SETUP`
/// entries whose builtins contain no config call either.
#[test]
fn prune_and_mktree_are_on_the_same_footing() {
    let (root, work, home) = packed_repo("settingsonly");
    let system = system_config(&root, "core.abbrev");

    let out = run(&work, &home, Some(&system), &["prune"]);
    assert_eq!(out.status.code(), Some(0), "prune: {}", stderr(&out));
    assert_eq!(stderr(&out), "", "prune");

    let out = run(&work, &home, Some(&system), &["mktree"]);
    assert_eq!(out.status.code(), Some(0), "mktree: {}", stderr(&out));
    assert_eq!(stderr(&out), "", "mktree");
    // The empty tree, which is what `mktree` with no input produces.
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
    );
}

/// `init-db` refuses, and writes nothing.
///
/// It is `cmd_init_db()` under its historical name and reads the same
/// configuration `init` does, so the valueless `core.abbrev` is fatal — the port
/// used to re-initialise the repository instead, a mutation git declines to make.
#[test]
fn init_db_refuses_and_leaves_the_repository_alone() {
    let (root, work, home) = packed_repo("initdb");
    let system = system_config(&root, "core.abbrev");
    let head_before = std::fs::read_to_string(work.join(".git/HEAD")).expect("HEAD");

    for verb in ["init-db", "init"] {
        let out = run(&work, &home, Some(&system), &[verb]);
        assert_eq!(out.status.code(), Some(128), "{verb}: {}", stderr(&out));
        assert_eq!(stderr(&out), nonbool_refusal(&system, "core.abbrev"), "{verb}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "", "{verb} prints nothing");
    }

    assert_eq!(
        std::fs::read_to_string(work.join(".git/HEAD")).expect("HEAD"),
        head_before,
        "the repository was not touched"
    );
}

/// The distinction the refusal turns on: a valueless key is fine for a *boolean*
/// reader and only fatal for one that wants a string or a number. `core.bare` and
/// `core.ignoreCase` are read with `git_config_bool()`, so a valueless spelling
/// of either is simply true and the command runs.
#[test]
fn a_valueless_boolean_key_is_true_not_an_error() {
    let (root, work, home) = packed_repo("boolean");

    for key in ["core.ignorecase", "core.logallrefupdates"] {
        let system = system_config(&root, key);
        let out = run(&work, &home, Some(&system), &["status", "-s"]);
        assert!(out.status.success(), "{key}: exit {:?} {}", out.status.code(), stderr(&out));
        assert_eq!(stderr(&out), "", "{key}");
    }
}

/// The refusal names the *file and line*, and does so for every key whose reader
/// wants something other than a boolean — the name in the message is the
/// lower-cased one the config parser produced, not the spelling in the file.
///
/// `core.createObject` is the one that shows the lower-casing: it is written in
/// camelCase and reported as `core.createobject`.
#[test]
fn every_nonbool_reader_names_the_file_and_line() {
    let (root, work, home) = packed_repo("nonbool");

    for (written, reported) in [
        ("core.checkstat", "core.checkstat"),
        ("diff.algorithm", "diff.algorithm"),
        ("core.createObject", "core.createobject"),
        ("core.abbrev", "core.abbrev"),
    ] {
        let system = system_config(&root, written);
        let out = run(&work, &home, Some(&system), &["status", "-s"]);
        assert_eq!(out.status.code(), Some(128), "{written}: {}", stderr(&out));
        assert_eq!(stderr(&out), nonbool_refusal(&system, reported), "{written}");
    }
}
