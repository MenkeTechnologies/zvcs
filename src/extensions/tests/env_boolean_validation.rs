//! `git_env_bool()` — which environment variables git *validates*, and which it
//! coerces without a word.
//!
//! `git_env_bool()` (parse.c:193-208) is the only reader for an environment
//! boolean in git, and it dies rather than falling back:
//!
//! ```c
//! int git_env_bool(const char *k, int def)
//! {
//!         const char *v = getenv(k);
//!         int val;
//!         if (!v)
//!                 return def;
//!         val = git_parse_maybe_bool(v);
//!         if (val < 0)
//!                 die(_("bad boolean environment value '%s' for '%s'"), v, k);
//!         return val;
//! }
//! ```
//!
//! The set that goes through it is closed and small, and **membership is the
//! whole test**: a variable git validates must be refused, and a variable git
//! merely reads with `getenv()` must keep being accepted, because turning a value
//! stock git accepts into a refusal is a worse divergence than the one it fixes.
//! So both directions are asserted below.
//!
//! Where each one fires is as load-bearing as whether it fires, because
//! `git_env_bool()` is called at the moment the value is *used*:
//!
//! | variable | C site | reached by |
//! |---|---|---|
//! | `GIT_CONFIG_NOSYSTEM` | `git_config_system()`, config.c:1541 | every invocation, `git --version` included |
//! | `GIT_DISCOVERY_ACROSS_FILESYSTEM` | `setup_git_directory_gently_1()`, setup.c:1597 | every verb that runs the discovery walk |
//! | `GIT_NO_LAZY_FETCH` | `setup_git_env_internal()`, setup.c:1066 | every verb that establishes a git directory |
//!
//! Every expectation is stock git 2.55.0's, captured by running it.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn scratch(tag: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let root = std::env::temp_dir().join(format!("zvcs-envbool-{tag}-{}-{unique}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home")).expect("mkdir fixture");
    std::fs::create_dir_all(root.join("repo")).expect("mkdir repo");
    std::fs::create_dir_all(root.join("plain")).expect("mkdir plain");
    root.canonicalize().expect("canonicalize fixture")
}

/// Run with one extra environment variable set. `GIT_CONFIG_NOSYSTEM` is only
/// defaulted when the test is not itself the thing setting it.
fn run(dir: &Path, home: &Path, extra: &[(&str, &str)], args: &[&str]) -> Output {
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
    if !extra.iter().any(|(k, _)| *k == "GIT_CONFIG_NOSYSTEM") {
        cmd.env("GIT_CONFIG_NOSYSTEM", "1");
    }
    for (k, v) in extra {
        cmd.env(k, v);
    }
    cmd.output().expect("run binary")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The one line `git_env_bool()` dies with.
fn refusal(key: &str, value: &str) -> String {
    format!("fatal: bad boolean environment value '{value}' for '{key}'\n")
}

fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let root = scratch(tag);
    let (home, repo, plain) = (root.join("home"), root.join("repo"), root.join("plain"));
    let out = run(&repo, &home, &[], &["init", "-q", "-b", "main"]);
    assert!(out.status.success(), "init: {}", stderr(&out));
    std::fs::write(repo.join("f.txt"), "hi\n").expect("write");
    assert!(run(&repo, &home, &[], &["add", "f.txt"]).status.success());
    assert!(run(&repo, &home, &[], &["commit", "-q", "-m", "one"]).status.success());
    (root, repo, home, plain)
}

/// `GIT_CONFIG_NOSYSTEM` is read by `git_config_system()`, which opens
/// `do_git_config_sequence()` — and that sequence runs from
/// `read_very_early_config()` inside `trace2_initialize()`, which `init_git()`
/// (common-init.c:77) performs before `cmd_main()` sees the command line.
///
/// So the refusal precedes *everything*, verbs that read no configuration
/// included. Measured against git 2.55.0 with the value `bogus`: all five of
/// these exit 128 with the same line, inside a repository and outside one.
#[test]
fn a_bad_config_nosystem_refuses_every_invocation() {
    let (_root, repo, home, plain) = fixture("nosystem");
    let bad = [("GIT_CONFIG_NOSYSTEM", "bogus")];

    for (dir, args) in [
        (&repo, vec!["--version"]),
        (&repo, vec!["status"]),
        (&repo, vec!["config", "--list"]),
        (&plain, vec!["config", "--list"]),
        (&plain, vec!["--exec-path"]),
    ] {
        let out = run(dir, &home, &bad, &args);
        assert_eq!(out.status.code(), Some(128), "{args:?}: {}", stderr(&out));
        assert_eq!(stderr(&out), refusal("GIT_CONFIG_NOSYSTEM", "bogus"), "{args:?}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "", "{args:?} prints nothing");
    }
}

/// It also *outranks* a malformed configuration file. The system scope is gated
/// before the user file is opened, so with both wrong git reports the environment
/// value and never names the file — confirmed against git 2.55.0 with `[]` as the
/// whole of `~/.gitconfig`.
#[test]
fn the_environment_refusal_precedes_the_config_file_refusal() {
    let root = scratch("order");
    let home = root.join("home");
    std::fs::write(home.join(".gitconfig"), "[]\n").expect("write global config");

    let mut cmd = Command::new(BIN);
    let out = cmd
        .args(["--version"])
        .current_dir(&root)
        .env("HOME", &home)
        .env("ZVCS_HOME", &home)
        .env("GIT_CONFIG_NOSYSTEM", "bogus")
        .env("LC_ALL", "C")
        .env_remove("GIT_CONFIG_GLOBAL")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("GIT_DIR")
        .output()
        .expect("run binary");

    assert_eq!(out.status.code(), Some(128), "{}", stderr(&out));
    assert_eq!(stderr(&out), refusal("GIT_CONFIG_NOSYSTEM", "bogus"));
}

/// The grammar is `git_parse_maybe_bool()`'s, not `str::parse::<bool>()`'s. Every
/// value here is one stock git accepts, and refusing any of them would break far
/// more than the refusal fixes — `GIT_CONFIG_NOSYSTEM=1` is what half the test
/// suites on earth set.
#[test]
fn the_boolean_grammar_is_gits_own() {
    let (_root, repo, home, _plain) = fixture("grammar");

    // Empty is *false*; the words are case-insensitive; the integer fallback is
    // the base-0 grammar, so `0x10` and `1k` are true and `0x0` is false.
    for value in ["", "0", "1", "true", "false", "yes", "no", "on", "off", "ON", "TRUE", "0x10", "0x0", "1k"] {
        let out = run(&repo, &home, &[("GIT_CONFIG_NOSYSTEM", value)], &["--version"]);
        assert_eq!(out.status.code(), Some(0), "GIT_CONFIG_NOSYSTEM={value:?}: {}", stderr(&out));
        assert_eq!(stderr(&out), "", "GIT_CONFIG_NOSYSTEM={value:?}");
    }

    // Past `int` range is not a boolean at all — `git_parse_int()` fails and
    // `git_parse_maybe_bool()` answers -1.
    for value in ["bogus", "99999999999999999999", "yeah"] {
        let out = run(&repo, &home, &[("GIT_CONFIG_NOSYSTEM", value)], &["--version"]);
        assert_eq!(out.status.code(), Some(128), "GIT_CONFIG_NOSYSTEM={value:?}");
        assert_eq!(stderr(&out), refusal("GIT_CONFIG_NOSYSTEM", value));
    }
}

/// `GIT_DISCOVERY_ACROSS_FILESYSTEM` is read at the top of the discovery loop, so
/// it refuses every verb that walks — including the gentle ones that would have
/// carried on without a repository, and including runs from outside a repository,
/// where the walk still happens and still reads the variable.
///
/// `git --version` and `git init` never walk, so they are unaffected. Both halves
/// measured against git 2.55.0.
#[test]
fn a_bad_discovery_across_filesystem_refuses_the_verbs_that_walk() {
    let (root, repo, home, plain) = fixture("discovery");
    let bad = [("GIT_DISCOVERY_ACROSS_FILESYSTEM", "bogus")];

    for (dir, args) in [
        (&repo, vec!["status"]),
        (&repo, vec!["rev-parse", "--git-dir"]),
        (&repo, vec!["config", "--list"]),
        (&plain, vec!["config", "--list"]),
        (&plain, vec!["var", "GIT_EDITOR"]),
    ] {
        let out = run(dir, &home, &bad, &args);
        assert_eq!(out.status.code(), Some(128), "{args:?}: {}", stderr(&out));
        assert_eq!(
            stderr(&out),
            refusal("GIT_DISCOVERY_ACROSS_FILESYSTEM", "bogus"),
            "{args:?}"
        );
    }

    let out = run(&repo, &home, &bad, &["--version"]);
    assert_eq!(out.status.code(), Some(0), "--version: {}", stderr(&out));

    let fresh = root.join("fresh");
    std::fs::create_dir_all(&fresh).expect("mkdir fresh");
    let out = run(&fresh, &home, &bad, &["init", "-q"]);
    assert_eq!(out.status.code(), Some(0), "init: {}", stderr(&out));
}

/// `GIT_NO_LAZY_FETCH` is read by `setup_git_env_internal()`, which runs once a
/// git directory has been *established* — so unlike the discovery variable it is
/// silent when there is no repository to set up, and fires for `git init`, which
/// creates one.
#[test]
fn a_bad_no_lazy_fetch_refuses_only_once_a_repository_exists() {
    let (root, repo, home, plain) = fixture("lazyfetch");
    let bad = [("GIT_NO_LAZY_FETCH", "bogus")];

    for args in [vec!["status"], vec!["log", "--oneline"], vec!["rev-parse", "--git-dir"]] {
        let out = run(&repo, &home, &bad, &args);
        assert_eq!(out.status.code(), Some(128), "{args:?}: {}", stderr(&out));
        assert_eq!(stderr(&out), refusal("GIT_NO_LAZY_FETCH", "bogus"), "{args:?}");
    }

    // Outside a repository nothing is set up, so nothing reads the variable.
    for args in [vec!["config", "--list"], vec!["var", "GIT_EDITOR"]] {
        let out = run(&plain, &home, &bad, &args);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {}", stderr(&out));
        assert_eq!(stderr(&out), "", "{args:?}");
    }

    // `git init` creates the directory and then sets the environment up for it.
    let fresh = root.join("fresh-lazy");
    std::fs::create_dir_all(&fresh).expect("mkdir fresh");
    let out = run(&fresh, &home, &bad, &["init", "-q"]);
    assert_eq!(out.status.code(), Some(128), "init: {}", stderr(&out));
    assert_eq!(stderr(&out), refusal("GIT_NO_LAZY_FETCH", "bogus"));
}

/// The other direction, and the reason the list above is a list rather than a
/// rule: these variables are read with a plain `getenv()` presence test or are
/// not booleans at all, so stock git takes `bogus` without a word. Refusing any
/// of them would be over-validation — a refusal git does not have.
///
/// Each pairing is with a verb that actually reads the variable, so a silent
/// `Ok` here means "git looked at it and did not mind", not "git never looked".
#[test]
fn the_variables_git_coerces_are_still_coerced() {
    let (_root, repo, home, _plain) = fixture("coerced");

    for (key, args) in [
        // `read_replace_refs` is a presence test in `setup_git_env_internal()`.
        ("GIT_NO_REPLACE_OBJECTS", vec!["log", "--oneline"]),
        // `core.skipHash`'s environment override, read as a string.
        ("GIT_SKIP_HASH", vec!["status"]),
        // `git_env_bool(GIT_IMPLICIT_WORK_TREE_ENVIRONMENT, 1)` sits behind an
        // `else if` that a plain discovered repository never reaches.
        ("GIT_IMPLICIT_WORK_TREE", vec!["rev-parse", "--show-toplevel"]),
        // Behind `if (!oideq(oid, null_oid(…)))`, so a healthy index never asks.
        ("GIT_ALLOW_NULL_SHA1", vec!["status"]),
    ] {
        let out = run(&repo, &home, &[(key, "bogus")], &args);
        assert_eq!(out.status.code(), Some(0), "{key} with {args:?}: {}", stderr(&out));
        assert!(
            !stderr(&out).contains("bad boolean environment value"),
            "{key} must not be validated: {:?}",
            stderr(&out)
        );
    }
}
