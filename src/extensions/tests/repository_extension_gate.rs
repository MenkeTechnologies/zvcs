//! `verify_repository_format()` — the two extension arms, and the versions they
//! apply to.
//!
//! `check_repo_format()` (setup.c:720-749) sorts every `extensions.<name>` it
//! sees into one of three places, and `verify_repository_format()`
//! (setup.c:881-917) then reports two of them under opposite conditions:
//!
//! ```c
//! if (format->version >= 1 && format->unknown_extensions.nr) {
//!         strbuf_addstr(err, Q_("unknown repository extension found:",
//!                               "unknown repository extensions found:",
//!                               format->unknown_extensions.nr));
//!         …
//! }
//!
//! if (format->version == 0 && format->v1_only_extensions.nr) {
//!         strbuf_addstr(err, Q_("repo version is 0, but v1-only extension found:", …));
//!         …
//! }
//! ```
//!
//! The two arms are each other's inverse, which is the property most easily got
//! wrong: an extension git has never heard of is **fatal at version 1 and
//! silently ignored at version 0**, while a known v1-only extension is the other
//! way round. A third list, `handle_extension_v0()` (setup.c:614-634), is
//! accepted at any version at all "for historical compatibility" and never
//! reaches either report.
//!
//! `gix` diagnoses only one of these — `extensions.objectFormat` at version 0 —
//! so every other shape read as a perfectly good repository and every verb ran
//! and exited 0 where stock git exits 128.
//!
//! Every expectation is stock git 2.55.0's, captured by running it against the
//! same fixtures. The refusal is asserted for `RUN_SETUP` verbs only: git calls
//! `check_repository_format_gently()` with a non-`NULL` `nongit_ok` for its
//! `RUN_SETUP_GENTLY` class, which downgrades the same message to a `warning:`
//! and runs the command outside the repository — a discovery restructuring this
//! port has not made, and one this gate must therefore not pre-empt with a
//! `fatal:` git does not raise. See `dispatch::FORMAT_GENTLE_VERBS`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn scratch(tag: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let root = std::env::temp_dir().join(format!("zvcs-repofmt-{tag}-{}-{unique}", std::process::id()));
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
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("XDG_CONFIG_HOME")
        .output()
        .expect("run binary")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A repository whose `.git/config` carries `core.repositoryformatversion =
/// <version>` and the given `[extensions]` body.
fn repo_with(tag: &str, version: i32, extensions: &str) -> (PathBuf, PathBuf) {
    let root = scratch(tag);
    let (home, work) = (root.join("home"), root.join("wk"));
    std::fs::create_dir_all(&work).expect("mkdir work");
    let out = run(&work, &home, &["init", "-q", "-b", "main"]);
    assert!(out.status.success(), "init: {}", stderr(&out));

    let config = work.join(".git/config");
    let text = std::fs::read_to_string(&config).expect("read config");
    let rewritten = text.replace(
        "repositoryformatversion = 0",
        &format!("repositoryformatversion = {version}"),
    );
    assert_ne!(rewritten, text, "the fixture must have rewritten the version");
    std::fs::write(&config, format!("{rewritten}{extensions}")).expect("write config");
    (work, home)
}

/// Every `RUN_SETUP` verb dies with the same message, before doing anything.
fn assert_refuses(work: &Path, home: &Path, expected: &str) {
    for args in [
        vec!["status"],
        vec!["rev-parse", "--git-dir"],
        vec!["log", "--oneline"],
        vec!["for-each-ref"],
    ] {
        let out = run(work, home, &args);
        assert_eq!(out.status.code(), Some(128), "{args:?}: {}", stderr(&out));
        assert_eq!(stderr(&out), format!("fatal: {expected}\n"), "{args:?}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "", "{args:?} prints nothing");
    }
}

/// An `extensions.*` git does not know, at repository format version 1.
///
/// This is the arm `gix` has no error for at all, so the port read the
/// repository happily and every verb exited 0.
#[test]
fn an_unknown_extension_at_version_one_is_fatal() {
    let (work, home) = repo_with("unknown-v1", 1, "[extensions]\n\tbogus = 1\n");
    assert_refuses(&work, &home, "unknown repository extension found:\n\tbogus");
}

/// `Q_()` switches the first line to the plural, and each offender follows on its
/// own tab-indented line, in file order.
#[test]
fn two_unknown_extensions_use_the_plural_and_list_both() {
    let (work, home) = repo_with(
        "unknown-two",
        1,
        "[extensions]\n\tbogus = 1\n\tsecondbogus = 1\n",
    );
    assert_refuses(
        &work,
        &home,
        "unknown repository extensions found:\n\tbogus\n\tsecondbogus",
    );
}

/// The inverse condition, and the one most easily got backwards: the *same*
/// unknown extension at version 0 is not an error at all. `verify_repository_format()`
/// only looks at `unknown_extensions` when the version is 1 or above.
#[test]
fn an_unknown_extension_at_version_zero_is_ignored() {
    let (work, home) = repo_with("unknown-v0", 0, "[extensions]\n\tbogus = 1\n");

    let out = run(&work, &home, &["rev-parse", "--git-dir"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stderr(&out), "");
    assert_eq!(String::from_utf8_lossy(&out.stdout), ".git\n");
}

/// A *known* v1-only extension at version 0 — the other arm. `noop-v1` exists in
/// `handle_extension()` for exactly this test and does nothing else, so the
/// refusal is entirely about the version.
#[test]
fn a_v1_only_extension_at_version_zero_is_fatal() {
    let (work, home) = repo_with("v1only", 0, "[extensions]\n\tnoop-v1 = 1\n");
    assert_refuses(
        &work,
        &home,
        "repo version is 0, but v1-only extension found:\n\tnoop-v1",
    );
}

/// The name in the message is the config parser's, already lower-cased —
/// `extensions.objectFormat` is reported as `objectformat`.
#[test]
fn the_reported_name_is_the_lower_cased_one() {
    let (work, home) = repo_with("case", 0, "[extensions]\n\tobjectFormat = sha256\n");
    assert_refuses(
        &work,
        &home,
        "repo version is 0, but v1-only extension found:\n\tobjectformat",
    );
}

/// `handle_extension_v0()`'s four are accepted at any version and never listed —
/// they are consumed before either offender list is reached.
#[test]
fn the_v0_extensions_are_accepted_at_version_zero() {
    for name in ["noop", "preciousObjects", "worktreeConfig"] {
        let (work, home) = repo_with(
            &format!("v0ext-{}", name.to_ascii_lowercase()),
            0,
            &format!("[extensions]\n\t{name} = true\n"),
        );
        let out = run(&work, &home, &["rev-parse", "--git-dir"]);
        assert_eq!(out.status.code(), Some(0), "{name}: {}", stderr(&out));
        assert_eq!(stderr(&out), "", "{name}");
    }
}

/// `GIT_REPO_VERSION_READ` is 1, so anything above it is refused before either
/// extension arm is consulted — the version check is the first thing
/// `verify_repository_format()` does.
#[test]
fn a_version_above_one_is_refused_first() {
    let (work, home) = repo_with("v2", 2, "[extensions]\n\tbogus = 1\n");
    assert_refuses(&work, &home, "Expected git repo version <= 1, found 2");
}

/// A repository whose config names no version at all is a silent success, whatever
/// its extensions say. `read_repository_format()` leaves `version` at -1 and
/// `check_repository_format_gently()` (setup.c:766-769) returns 0 for it —
/// "we treat a missing config as a silent ok".
#[test]
fn a_config_without_a_version_is_a_silent_ok() {
    let root = scratch("noversion");
    let (home, work) = (root.join("home"), root.join("wk"));
    std::fs::create_dir_all(&work).expect("mkdir work");
    assert!(run(&work, &home, &["init", "-q", "-b", "main"]).status.success());

    let config = work.join(".git/config");
    let text = std::fs::read_to_string(&config).expect("read config");
    let stripped: String = text
        .lines()
        .filter(|l| !l.contains("repositoryformatversion"))
        .map(|l| format!("{l}\n"))
        .collect();
    std::fs::write(&config, format!("{stripped}[extensions]\n\tnoop-v1 = 1\n")).expect("write");

    let out = run(&work, &home, &["rev-parse", "--git-dir"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stderr(&out), "");
}

/// The command-line scope is not the repository's format. `read_repository_format()`
/// reads `$GIT_COMMON_DIR/config` and nothing else, so `-c` cannot make a
/// repository unreadable — measured against git 2.55.0, which accepts this.
#[test]
fn a_command_line_extension_is_not_the_repositorys_format() {
    let root = scratch("cmdline");
    let (home, work) = (root.join("home"), root.join("wk"));
    std::fs::create_dir_all(&work).expect("mkdir work");
    assert!(run(&work, &home, &["init", "-q", "-b", "main"]).status.success());

    // The repository is at version 1 so the *unknown extension* arm is live; the
    // only thing missing is a repository-scope extension for it to complain about.
    let config = work.join(".git/config");
    let text = std::fs::read_to_string(&config).expect("read config");
    let rewritten = text.replace(
        "repositoryformatversion = 0",
        "repositoryformatversion = 1",
    );
    assert_ne!(rewritten, text, "the fixture must have rewritten the version");
    std::fs::write(&config, rewritten).expect("write config");

    let out = run(&work, &home, &["-c", "extensions.bogus=1", "rev-parse", "--git-dir"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stderr(&out), "");
}
