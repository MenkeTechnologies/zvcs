//! `fatal: bad config line <n> in file <path>` — the refusal that only a config
//! *file* can reach.
//!
//! `git_parse_source()` (config.c:1141-1170) builds the message from the source
//! it was reading and `do_config_from_file()` (config.c:1394) sets
//! `top.default_error_action = CONFIG_ERROR_DIE`, so every on-disk file in
//! `do_git_config_sequence()` dies where it stands, at exit 128. None of this is
//! reachable through `-c`: a command-line override never goes through a file, so
//! the whole class was invisible until the parity harness learned to deliver
//! sampled configuration through the real scopes.
//!
//! There are two moments, and they cover different commands:
//!
//!   * `read_very_early_config()`, reached from `tr2_sysenv_load()` inside
//!     `trace2_initialize()` — which `init_git()` (common-init.c:77) runs before
//!     `cmd_main()`. It reads system, XDG and user configuration, so a malformed
//!     one of those refuses *every* invocation, `git --version` included.
//!   * the repository half, read by `run_builtin()` (git.c:479-491) through
//!     `setup_git_directory()` and `check_pager_config()` before the builtin's
//!     own `fn` runs. It applies to every `RUN_SETUP`/`RUN_SETUP_GENTLY` entry of
//!     the `commands[]` table and to nothing else — which is why `git
//!     merge-index` dies at 128 instead of printing its usage at 129, even though
//!     `cmd_merge_index()` (builtin/merge-index.c:81-131) contains no config call
//!     at all.
//!
//! Every expectation below was captured from stock git 2.55.0 on the same input
//! and is asserted as a literal, so these run headless with nothing on `PATH` but
//! the binary under test.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A scratch directory plus an isolated `HOME`, so no ambient configuration can
/// reach a run. Named per test and per pid so concurrent binaries never share one.
fn scratch(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-cfgrefuse-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home")).expect("mkdir fixture");
    root.canonicalize().expect("canonicalize fixture")
}

/// Run the binary under test with every configuration scope disabled unless the
/// test opts into one.
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

/// [`run`] with an extra environment pair, for the tests that aim a scope at a
/// file of their own. An empty value *removes* the variable, which is how a test
/// asks for the scope's own default rather than an override.
fn run_env(dir: &Path, home: &Path, env: &[(&str, &str)], args: &[&str]) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("XDG_CONFIG_HOME");
    for (k, v) in env {
        if v.is_empty() {
            cmd.env_remove(k);
        } else {
            cmd.env(k, v);
        }
    }
    cmd.output().expect("run zvcs git")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A repository with a clean `.git/config`, and a copy of it to restore between
/// the malformed variants.
fn repo(tag: &str) -> (PathBuf, PathBuf) {
    let root = scratch(tag);
    let (home, work) = (root.join("home"), root.join("wk"));
    std::fs::create_dir_all(&work).expect("mkdir work");
    let out = run(&work, &home, &["init", "-q", "-b", "main"]);
    assert!(out.status.success(), "init: {}", stderr(&out));
    let cfg = work.join(".git/config");
    std::fs::copy(&cfg, work.join(".git/config.pristine")).expect("save pristine config");
    (work, home)
}

/// Restore the pristine config and append `tail` to it. Returns the 1-based line
/// `tail` starts on, which is the line git's message names.
fn append_to_repo_config(work: &Path, tail: &str) -> usize {
    let cfg = work.join(".git/config");
    let pristine = std::fs::read_to_string(work.join(".git/config.pristine")).expect("pristine");
    let line = pristine.lines().count() + 1;
    std::fs::write(&cfg, format!("{pristine}{tail}")).expect("write config");
    line
}

// ---------------------------------------------------------------------------
// The malformed forms
// ---------------------------------------------------------------------------

/// Seven spellings git's parser refuses, each naming the line it sits on.
///
/// The last two are the interesting ones. `[core]\n\tabbrev = "bad\qescape"`
/// fails in the *value* parser rather than the name parser, and `garbage line`
/// fails a rule gitoxide's parser does not have: `get_value()` reads the name,
/// skips spaces and tabs, and then requires `\n` or `=`, so a second word on the
/// line is `return -1` and not a second valueless key.
#[test]
fn every_malformed_form_names_its_line_and_exits_128() {
    let (work, home) = repo("forms");

    for tail in [
        "[]\n",
        "[bad section]\n",
        "[core \"a\" b]\n",
        "x = \"unterminated\n",
        "]\n",
        "garbage line\n",
    ] {
        let line = append_to_repo_config(&work, tail);
        let out = run(&work, &home, &["status", "-s"]);
        assert_eq!(out.status.code(), Some(128), "{tail:?}: {}", stderr(&out));
        assert_eq!(
            stderr(&out),
            format!("fatal: bad config line {line} in file .git/config\n"),
            "{tail:?}"
        );
    }

    // A bad escape inside a quoted value: the refusal lands on the value's line,
    // which is the second of the two appended.
    let line = append_to_repo_config(&work, "[core]\n\tabbrev = \"bad\\qescape\"\n");
    let out = run(&work, &home, &["status", "-s"]);
    assert_eq!(out.status.code(), Some(128), "{}", stderr(&out));
    assert_eq!(
        stderr(&out),
        format!("fatal: bad config line {} in file .git/config\n", line + 1)
    );
}

/// A valueless key is legal, and legal only at the end of its line.
///
/// git's `get_value()` allows `\n` (or EOF) after the name and nothing else — not
/// even a comment marker, which is the surprising half. All eight of these were
/// measured against git 2.55.0 through `git config --list` on the global file.
#[test]
fn a_valueless_key_must_end_its_line() {
    let (work, home) = repo("valueless");

    for tail in ["[a]\nb\n", "[a]\nb   \n", "[a]\nb\t\n", "[a]\nb"] {
        append_to_repo_config(&work, tail);
        let out = run(&work, &home, &["status", "-s"]);
        assert!(out.status.success(), "{tail:?} is legal: {}", stderr(&out));
    }

    for tail in ["[a]\nb ; c\n", "[a]\nb # c\n", "[a]\nb;c\n", "[a]\nb c\n"] {
        let line = append_to_repo_config(&work, tail);
        let out = run(&work, &home, &["status", "-s"]);
        assert_eq!(out.status.code(), Some(128), "{tail:?}: {}", stderr(&out));
        assert_eq!(
            stderr(&out),
            format!("fatal: bad config line {} in file .git/config\n", line + 1),
            "{tail:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Which commands the two moments cover
// ---------------------------------------------------------------------------

/// The repository scope refuses every verb that takes repository setup — its
/// usage error, its `-h`, and its own diagnostics all come after.
///
/// `merge-index` is the one that motivated this: it is `RUN_SETUP | NO_PARSEOPT`
/// and reads no configuration of its own, so bare it used to answer 129 with
/// `usage: git merge-index …` while git answers 128.
#[test]
fn the_repository_scope_refuses_before_the_verb_runs() {
    let (work, home) = repo("verbs");
    let line = append_to_repo_config(&work, "[]\n");
    let expected = format!("fatal: bad config line {line} in file .git/config\n");

    for args in [
        vec!["merge-index"],
        vec!["status"],
        vec!["status", "-h"],
        vec!["status", "--nonsense"],
        vec!["log"],
        vec!["diff-files"],
        vec!["add"],
        vec!["commit"],
        vec!["mv"],
        vec!["ls-tree"],
        vec!["symbolic-ref"],
        vec!["cherry-pick"],
        vec!["rev-parse"],
        vec!["hash-object"],
        vec!["shortlog"],
        vec!["prune-packed"],
        vec!["update-ref"],
        vec!["for-each-ref"],
    ] {
        let out = run(&work, &home, &args);
        assert_eq!(out.status.code(), Some(128), "{args:?}: {}", stderr(&out));
        assert_eq!(stderr(&out), expected, "{args:?}");
    }
}

/// The three verbs `commands[]` gives neither `RUN_SETUP` nor `RUN_SETUP_GENTLY`
/// and that read no configuration on their own keep working. Measured: with `[]`
/// in `.git/config`, `git version`, `git help` and `git stripspace` exit 0.
#[test]
fn the_repository_scope_spares_the_verbs_that_never_read_it() {
    let (work, home) = repo("spared");
    append_to_repo_config(&work, "[]\n");

    for args in [vec!["version"], vec!["help"], vec!["stripspace"]] {
        let out = run(&work, &home, &args);
        assert!(out.status.success(), "{args:?}: exit {:?}", out.status.code());
        assert_eq!(stderr(&out), "", "{args:?}");
    }
}

/// The early scope has no such exceptions. `read_very_early_config()` runs inside
/// `init_git()`, before a single argument is looked at, so even the query globals
/// that `handle_options()` answers with `puts()` + `exit(0)` never get there.
#[test]
fn the_early_scope_refuses_even_the_query_globals() {
    let root = scratch("early");
    let (home, work) = (root.join("home"), root.join("wk"));
    std::fs::create_dir_all(&work).expect("mkdir work");
    let global = root.join("gitconfig");
    std::fs::write(&global, "[a]\nb = c\n[]\n").expect("write global");
    let global_str = global.to_str().expect("utf8 path");
    let expected = format!("fatal: bad config line 3 in file {global_str}\n");

    for args in [
        vec!["--version"],
        vec!["--exec-path"],
        vec!["--html-path"],
        vec!["--man-path"],
        vec!["version"],
        vec!["help"],
        vec!["stripspace"],
        vec!["merge-index"],
        vec!["status"],
    ] {
        let out = run_env(&work, &home, &[("GIT_CONFIG_GLOBAL", global_str)], &args);
        assert_eq!(out.status.code(), Some(128), "{args:?}: {}", stderr(&out));
        assert_eq!(stderr(&out), expected, "{args:?}");
    }
}

/// Each global scope is named by the absolute path git opened, and they are
/// consulted in `do_git_config_sequence()` order — system, then XDG, then user —
/// so the *first* unreadable one is the one named even when a later scope is
/// broken too.
#[test]
fn the_global_scopes_are_named_by_path_and_read_in_order() {
    let root = scratch("scopes");
    let (home, work) = (root.join("home"), root.join("wk"));
    std::fs::create_dir_all(&work).expect("mkdir work");
    std::fs::create_dir_all(home.join(".config/git")).expect("mkdir xdg");

    let system = root.join("system");
    std::fs::write(&system, "[]\n").expect("write system");
    let out = run_env(
        &work,
        &home,
        &[
            ("GIT_CONFIG_NOSYSTEM", "0"),
            ("GIT_CONFIG_SYSTEM", system.to_str().expect("utf8")),
        ],
        &["status", "-s"],
    );
    assert_eq!(out.status.code(), Some(128), "{}", stderr(&out));
    assert_eq!(
        stderr(&out),
        format!("fatal: bad config line 1 in file {}\n", system.display())
    );

    // XDG before user: both are broken, and the XDG one is named.
    let xdg = home.join(".config/git/config");
    std::fs::write(&xdg, "[x]\ny = z\n[]\n").expect("write xdg");
    std::fs::write(home.join(".gitconfig"), "[]\n").expect("write user");
    let out = run_env(&work, &home, &[("GIT_CONFIG_GLOBAL", "")], &["status", "-s"]);
    assert_eq!(out.status.code(), Some(128), "{}", stderr(&out));
    assert_eq!(
        stderr(&out),
        format!("fatal: bad config line 3 in file {}\n", xdg.display())
    );

    // With the XDG file gone, the user file is next.
    std::fs::remove_file(&xdg).expect("remove xdg");
    let out = run_env(&work, &home, &[("GIT_CONFIG_GLOBAL", "")], &["status", "-s"]);
    assert_eq!(out.status.code(), Some(128), "{}", stderr(&out));
    assert_eq!(
        stderr(&out),
        format!(
            "fatal: bad config line 1 in file {}\n",
            home.join(".gitconfig").display()
        )
    );
}

/// How `$GIT_DIR` is spelled in the message is not a constant.
///
/// `setup_git_directory()` chdirs to the top of the work tree and keeps the
/// relative `.git` the walk found, so the message says `.git/config` from
/// anywhere inside the work tree. `$GIT_DIR` set outright is repeated verbatim,
/// a `--separate-git-dir` repository names the directory it actually lives in,
/// and `cmd_init_db()` — which resolves the path itself rather than using that
/// setup — names the absolute one.
#[test]
fn the_repository_path_is_spelled_the_way_setup_left_it() {
    let (work, home) = repo("paths");
    let line = append_to_repo_config(&work, "[]\n");
    let relative = format!("fatal: bad config line {line} in file .git/config\n");

    // From the top, and from a subdirectory: git chdirs to the top either way.
    let out = run(&work, &home, &["status", "-s"]);
    assert_eq!(stderr(&out), relative);
    let sub = work.join("sub");
    std::fs::create_dir_all(&sub).expect("mkdir sub");
    let out = run(&sub, &home, &["status", "-s"]);
    assert_eq!(stderr(&out), relative, "a subdirectory still names .git/config");

    // `$GIT_DIR` is repeated exactly as given — relative stays relative.
    let out = run_env(&work, &home, &[("GIT_DIR", ".git")], &["status", "-s"]);
    assert_eq!(stderr(&out), relative);

    let git_dir = work.join(".git");
    let out = run_env(
        &work,
        &home,
        &[("GIT_DIR", git_dir.to_str().expect("utf8"))],
        &["status", "-s"],
    );
    assert_eq!(
        stderr(&out),
        format!("fatal: bad config line {line} in file {}/config\n", git_dir.display()),
        "an absolute GIT_DIR is repeated absolutely"
    );

    // `git init` resolves the directory for itself, so it names the absolute path
    // where `status` names the relative one.
    let out = run(&work, &home, &["init"]);
    assert_eq!(out.status.code(), Some(128), "{}", stderr(&out));
    assert_eq!(
        stderr(&out),
        format!("fatal: bad config line {line} in file {}/config\n", git_dir.display())
    );
    let out = run(&work, &home, &["init-db"]);
    assert_eq!(
        stderr(&out),
        format!("fatal: bad config line {line} in file {}/config\n", git_dir.display()),
        "init-db is cmd_init_db under its historical name"
    );
}

/// `$GIT_DIR/config.worktree` is the fifth and last file of the sequence, read
/// only when `extensions.worktreeConfig` is on, and named as its own path.
#[test]
fn the_worktree_scope_is_named_separately() {
    let (work, home) = repo("worktree");
    let out = run(&work, &home, &["config", "--local", "extensions.worktreeConfig", "true"]);
    assert!(out.status.success(), "{}", stderr(&out));
    std::fs::write(work.join(".git/config.worktree"), "[]\n").expect("write worktree config");

    let out = run(&work, &home, &["status", "-s"]);
    assert_eq!(out.status.code(), Some(128), "{}", stderr(&out));
    assert_eq!(stderr(&out), "fatal: bad config line 1 in file .git/config.worktree\n");
}

/// `include.path` is followed where git follows it, so an included file that will
/// not parse is named by its own path rather than the includer's.
#[test]
fn a_broken_include_is_named_by_its_own_path() {
    let (work, home) = repo("include");
    let included = work.join("extra.config");
    std::fs::write(&included, "[a]\nb = c\n]\n").expect("write include target");
    append_to_repo_config(
        &work,
        &format!("[include]\n\tpath = {}\n", included.to_str().expect("utf8")),
    );

    let out = run(&work, &home, &["status", "-s"]);
    assert_eq!(out.status.code(), Some(128), "{}", stderr(&out));
    assert_eq!(
        stderr(&out),
        format!("fatal: bad config line 3 in file {}\n", included.display())
    );
}

/// The value parser is not what is being fixed here, and `-c` must keep behaving
/// as it did: a command-line override goes through `git_config_from_parameters()`
/// with no file behind it, so it can neither reach this diagnostic nor be spared
/// by it.
#[test]
fn command_line_overrides_are_untouched() {
    let (work, home) = repo("cli");

    // A well-formed override on a clean repository still works.
    let out = run(&work, &home, &["-c", "core.abbrev=8", "status", "-s"]);
    assert!(out.status.success(), "{}", stderr(&out));

    // And an override cannot rescue a file that will not parse.
    let line = append_to_repo_config(&work, "[]\n");
    let out = run(&work, &home, &["-c", "core.abbrev=8", "status", "-s"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        stderr(&out),
        format!("fatal: bad config line {line} in file .git/config\n")
    );
}
