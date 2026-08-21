//! `read_and_verify_repository_format()` — the check that reads one file and one
//! file only.
//!
//! `read_repository_format()` (setup.c:866-876) is handed
//! `$GIT_COMMON_DIR/config` by its caller (setup.c:759-761) and parses it with
//! `git_config_from_file(check_repo_format, path, format)`. There is no configset
//! and no sequence: a command-line override never reaches it. So
//! `extensions.objectFormat = sha256` at `core.repositoryFormatVersion = 0` is
//! refused when it sits in `.git/config` and accepted when it arrives through
//! `-c`, and the difference is not a bug in either direction — it is the whole
//! point of the function.
//!
//! `verify_repository_format()` (setup.c:888-925) turns the parsed format into
//! the message, appending each offender as `"\n\t%s"`:
//!
//! ```c
//! if (format->version == 0 && format->v1_only_extensions.nr) {
//!         strbuf_addstr(err,
//!                       Q_("repo version is 0, but v1-only extension found:",
//!                          "repo version is 0, but v1-only extensions found:",
//!                          format->v1_only_extensions.nr));
//!
//!         for (i = 0; i < format->v1_only_extensions.nr; i++)
//!                 strbuf_addf(err, "\n\t%s",
//!                             format->v1_only_extensions.items[i].string);
//!         return -1;
//! }
//! ```
//!
//! The extension name is the lower-cased suffix the config parser produced, not
//! the spelling in the file. Every expectation was captured from stock git 2.55.0.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn scratch(tag: &str) -> PathBuf {
    // Unique per test *and* per run: these tests run concurrently and each one
    // wipes its own root on entry, so a shared name would let one test delete
    // another's repository mid-command.
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let root = std::env::temp_dir().join(format!(
        "zvcs-repofmt-{tag}-{}-{unique}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home")).expect("mkdir fixture");
    root.canonicalize().expect("canonicalize fixture")
}

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("XDG_CONFIG_HOME")
        .output()
        .expect("run zvcs git")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A repository, plus a pristine copy of its config to rewrite between cases.
fn repo(tag: &str) -> (PathBuf, PathBuf) {
    let root = scratch(tag);
    let (home, work) = (root.join("home"), root.join("wk"));
    std::fs::create_dir_all(&work).expect("mkdir work");
    let out = run(&work, &home, &["init", "-q", "-b", "main"]);
    assert!(out.status.success(), "init: {}", stderr(&out));
    std::fs::copy(work.join(".git/config"), work.join(".git/config.pristine")).expect("save config");
    (work, home)
}

/// Rewrite `.git/config` from the pristine copy, with `core.repositoryformatversion`
/// forced to `version` and `tail` appended.
fn set_format(work: &Path, version: u32, tail: &str) {
    let pristine = std::fs::read_to_string(work.join(".git/config.pristine")).expect("pristine");
    let rewritten = pristine.replace(
        "repositoryformatversion = 0",
        &format!("repositoryformatversion = {version}"),
    );
    std::fs::write(work.join(".git/config"), format!("{rewritten}{tail}")).expect("write config");
}

/// The two-line refusal, byte for byte, for every verb that reaches it.
///
/// The list spans the three ways a verb can arrive: the ordinary ones that open
/// the repository, `rev-parse` which answers off the discovery walk alone, and
/// `init`/`init-db` which create rather than open. All three were measured at 128
/// with the same message.
#[test]
fn a_v1_only_extension_at_version_0_is_a_two_line_fatal() {
    let (work, home) = repo("v1only");
    set_format(&work, 0, "[extensions]\n\tobjectFormat = sha256\n");
    let expected = "fatal: repo version is 0, but v1-only extension found:\n\tobjectformat\n";

    for args in [
        vec!["status", "-s"],
        vec!["log"],
        vec!["ls-files"],
        vec!["add", "."],
        vec!["branch"],
        vec!["describe"],
        vec!["count-objects"],
        vec!["diff-files"],
        vec!["rev-parse"],
        vec!["rev-parse", "--git-dir"],
        vec!["rev-parse", "HEAD"],
        vec!["init"],
        vec!["init-db"],
    ] {
        let out = run(&work, &home, &args);
        assert_eq!(out.status.code(), Some(128), "{args:?}: {}", stderr(&out));
        assert_eq!(stderr(&out), expected, "{args:?}");
    }
}

/// `GIT_REPO_VERSION_READ` is 1, so a higher version is refused first — before
/// the extension lists are even looked at.
#[test]
fn a_version_above_1_is_refused_with_its_own_message() {
    let (work, home) = repo("version2");
    set_format(&work, 2, "");
    let expected = "fatal: Expected git repo version <= 1, found 2\n";

    for args in [
        vec!["status", "-s"],
        vec!["log"],
        vec!["rev-parse", "--git-dir"],
        vec!["init"],
    ] {
        let out = run(&work, &home, &args);
        assert_eq!(out.status.code(), Some(128), "{args:?}: {}", stderr(&out));
        assert_eq!(stderr(&out), expected, "{args:?}");
    }
}

/// The same key through `-c` is accepted, because `read_repository_format()`
/// reads the file and nothing else. This is the assertion that keeps a fix to the
/// file path from being "fixed" by making the check global.
#[test]
fn the_same_extension_through_dash_c_is_accepted() {
    let (work, home) = repo("viacli");
    set_format(&work, 0, "");

    for args in [
        vec!["-c", "extensions.objectFormat=sha256", "status", "-s"],
        vec!["-c", "extensions.objectFormat=sha256", "ls-files"],
        vec!["-c", "core.repositoryFormatVersion=2", "status", "-s"],
    ] {
        let out = run(&work, &home, &args);
        assert!(out.status.success(), "{args:?}: exit {:?} {}", out.status.code(), stderr(&out));
        assert_eq!(stderr(&out), "", "{args:?}");
    }
}

/// `handle_extension_v0()` (setup.c:612-633) keeps four extensions working at
/// version 0 for historical compatibility, and an extension nothing recognises is
/// simply ignored there — only `unknown_extensions` at version 1 is fatal, which
/// is a different branch. So a v0 repository with an unknown extension, or with
/// `extensions.worktreeConfig`, still runs.
#[test]
fn version_0_ignores_what_it_does_not_recognise() {
    let (work, home) = repo("v0ok");

    for tail in [
        "[extensions]\n\tsomethingNobodyKnows = 1\n",
        "[extensions]\n\tworktreeConfig = false\n",
        "[extensions]\n\tpreciousObjects = false\n",
    ] {
        set_format(&work, 0, tail);
        let out = run(&work, &home, &["status", "-s"]);
        assert!(out.status.success(), "{tail:?}: exit {:?} {}", out.status.code(), stderr(&out));
        assert_eq!(stderr(&out), "", "{tail:?}");
    }
}

/// A clean repository must be silent — the gate that raises the refusal runs on
/// every invocation, so its no-op path is worth pinning too.
#[test]
fn a_well_formed_repository_is_untouched() {
    let (work, home) = repo("clean");
    set_format(&work, 0, "");

    for args in [vec!["status", "-s"], vec!["init"], vec!["init-db"], vec!["rev-parse", "--git-dir"]] {
        let out = run(&work, &home, &args);
        assert!(out.status.success(), "{args:?}: exit {:?} {}", out.status.code(), stderr(&out));
        assert_eq!(stderr(&out), "", "{args:?}");
    }

    // And version 1 with the extension it is for is a working repository as far
    // as the format check is concerned.
    set_format(&work, 1, "[extensions]\n\tnoop-v1 = true\n");
    let out = run(&work, &home, &["status", "-s"]);
    assert!(out.status.success(), "exit {:?}: {}", out.status.code(), stderr(&out));
}
