//! How `cmd_fetch()` settles `config.recurse_submodules` (builtin/fetch.c:2607-2662):
//! `submodule.recurse` / `fetch.recurseSubmodules` through `git_fetch_config()`, the
//! command line over them, and the `--negotiate-only` / `--porcelain` refusals, which
//! read the command line alone and switch a configured value off instead.
//!
//! Every expectation was captured from git 2.55.0 against the same fixture shape: a
//! superproject clone whose submodule's upstream has a commit the clone lacks.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(["-c", "protocol.file.allow=always"])
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("ZVCS_HOME", home)
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .output()
        .unwrap()
}

fn ok(dir: &Path, home: &Path, args: &[&str]) {
    let out = run(dir, home, args);
    assert!(
        out.status.success(),
        "setup `git {args:?}` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// `sub` (upstream of the submodule), `sup` (superproject), and `clone`, a
/// `--recurse-submodules` clone of `sup` whose `sub` upstream then gains a commit.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-fetch-recurse-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home")).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    ok(&root, &home, &["init", "-q", "-b", "main", "sub"]);
    ok(&root.join("sub"), &home, &["commit", "-q", "--allow-empty", "-m", "s1"]);
    ok(&root, &home, &["init", "-q", "-b", "main", "sup"]);
    ok(&root.join("sup"), &home, &["submodule", "add", "-q", "../sub", "sub"]);
    ok(&root.join("sup"), &home, &["commit", "-q", "-m", "add sub"]);
    ok(&root, &home, &["clone", "-q", "--recurse-submodules", "sup", "clone"]);
    ok(&root.join("sub"), &home, &["commit", "-q", "--allow-empty", "-m", "s2"]);
    (root.join("clone"), home)
}

#[test]
fn configured_recursion_uses_the_boolean_grammar_and_submodule_recurse() {
    let (clone, home) = fixture("config");
    // `git_config_bool()` for `submodule.recurse`, `parse_fetch_recurse()` for
    // `fetch.recurseSubmodules` — neither is limited to the lowercase spellings.
    for assignment in ["submodule.recurse=true", "fetch.recurseSubmodules=TRUE", "fetch.recurseSubmodules=2"] {
        let out = run(&clone, &home, &["-c", assignment, "fetch"]);
        assert!(out.status.success(), "for {assignment}: {}", stderr(&out));
        assert!(
            stderr(&out).contains("Fetching submodule sub\n"),
            "for {assignment}: {}",
            stderr(&out)
        );
    }
    // The command line takes the same grammar.
    let out = run(&clone, &home, &["fetch", "--recurse-submodules=on"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stderr(&out).contains("Fetching submodule sub\n"), "{}", stderr(&out));
}

#[test]
fn negotiate_only_and_porcelain_refuse_only_the_command_line() {
    let (clone, home) = fixture("refusals");

    // Refused straight after option parsing, ahead of the missing remote and tips.
    let out = run(&clone, &home, &["fetch", "--negotiate-only", "--recurse-submodules"]);
    assert_eq!(
        stderr(&out),
        "fatal: options '--negotiate-only' and '--recurse-submodules' cannot be used together\n"
    );
    assert_eq!(out.status.code(), Some(128));

    // `on-demand` is the `default:` arm too, and the porcelain refusal comes before
    // the `--depth`/`--deepen` cross-check.
    let out = run(
        &clone,
        &home,
        &["fetch", "--porcelain", "--recurse-submodules=on-demand", "--depth=1", "--deepen=1"],
    );
    assert_eq!(
        stderr(&out),
        "fatal: options '--porcelain' and '--recurse-submodules' cannot be used together\n"
    );
    assert_eq!(out.status.code(), Some(128));

    // A configured value is switched off rather than refused.
    let out = run(
        &clone,
        &home,
        &["-c", "fetch.recurseSubmodules=yes", "fetch", "--negotiate-only", "--negotiation-tip=main", "origin"],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let out = run(&clone, &home, &["-c", "fetch.recurseSubmodules=yes", "fetch", "--porcelain"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!stderr(&out).contains("Fetching submodule"), "{}", stderr(&out));
}
