//! The refusals repository setup raises from the *environment*, before a verb runs.
//!
//! Five gates and one diagnostic, all of them in `setup.c`/`config.c`/`transport.c`
//! and all of them reached before (or instead of) the command the user typed. Two
//! are security boundaries and the rest are diagnostics a scripted caller reads:
//!
//! * **`safe.directory`** (setup.c:1332-1456) — git refuses to operate on a
//!   repository owned by somebody else, because its hooks, its `core.fsmonitor`
//!   and its `core.pager` would otherwise run as us. `GIT_TEST_ASSUME_DIFFERENT_OWNER`
//!   is git's own hook for exercising it without a second account, which is what
//!   makes these cases runnable in CI.
//! * **`protocol.<name>.allow`** (transport.c:1047-1160) — which schemes a URL may
//!   reach. The `user` policy exists to separate "the user typed this URL" from
//!   "a `.gitmodules` file or a redirect produced it", and git marks the second
//!   with `GIT_PROTOCOL_FROM_USER=0`.
//! * **`$GIT_OBJECT_DIRECTORY`** (setup.c:433-442) — part of the test that decides
//!   whether a directory is a repository at all, so an unreachable one is reported
//!   as *no repository*, not as a broken object store.
//! * **`$GIT_CONFIG_COUNT`/`_KEY_`/`_VALUE_`** (config.c:731-780) — a malformed
//!   command-line override is two lines and exit 128, not one line and exit 1.
//! * **`$GIT_CONFIG_GLOBAL`** (config.c:1505-1537) — `--global` names one file, and
//!   `--list` dies when it cannot be read.
//! * **`$GIT_ALTERNATE_OBJECT_DIRECTORIES`** (odb.c:59-73) — a missing alternate is
//!   an `error()` and the command carries on.
//!
//! Every expectation below is bytes captured from stock git 2.55.0 run on the same
//! inputs. The accepting half of each gate is asserted alongside the refusing
//! half: a gate that always refuses is as wrong as one that never does.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A scratch tree with `home/` (an empty `$HOME`, so the developer's own
/// `safe.directory` entries cannot leak in) and `work/` — a repository with one
/// commit and one subdirectory.
struct Fixture {
    root: PathBuf,
    home: PathBuf,
    work: PathBuf,
}

fn fixture(tag: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!("zvcs-setupenv-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::fs::create_dir_all(root.join("work/sub")).unwrap();
    // macOS reaches the temp directory through a symlink and both binaries record
    // the resolved path, so every expectation has to be built from the resolved
    // root — the ownership message quotes the work tree it discovered.
    let root = root.canonicalize().unwrap();
    let f = Fixture {
        home: root.join("home"),
        work: root.join("work"),
        root,
    };
    ok(&f, &f.work, &["init", "-q", "-b", "main"]);
    std::fs::write(f.work.join("a.txt"), "a\n").unwrap();
    ok(&f, &f.work, &["add", "a.txt"]);
    ok(&f, &f.work, &["commit", "-q", "-m", "one"]);
    f
}

/// The binary, with the ambient environment scrubbed back to a known state.
fn run(f: &Fixture, dir: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .current_dir(dir)
        .env("HOME", &f.home)
        .env("ZVCS_HOME", &f.home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_CEILING_DIRECTORIES")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_CONFIG_GLOBAL")
        // The XDG fallback is `$XDG_CONFIG_HOME/git/config` when that
        // variable is set and `$HOME/.config/git/config` only when it is not.
        // The ubuntu runner exports it, so a fixture writing under the pinned
        // HOME wrote a file nothing would read and `--global --list` came back
        // empty there while passing on macOS, which exports no such variable.
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_ALLOW_PROTOCOL")
        .env_remove("GIT_PROTOCOL_FROM_USER")
        .env_remove("GIT_TEST_ASSUME_DIFFERENT_OWNER");
    cmd.output().expect("run binary")
}

/// The same, with extra environment on top.
fn run_env(f: &Fixture, dir: &Path, env: &[(&str, &str)], args: &[&str]) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .current_dir(dir)
        .env("HOME", &f.home)
        .env("ZVCS_HOME", &f.home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_CEILING_DIRECTORIES")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_CONFIG_GLOBAL")
        // The XDG fallback is `$XDG_CONFIG_HOME/git/config` when that
        // variable is set and `$HOME/.config/git/config` only when it is not.
        // The ubuntu runner exports it, so a fixture writing under the pinned
        // HOME wrote a file nothing would read and `--global --list` came back
        // empty there while passing on macOS, which exports no such variable.
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_ALLOW_PROTOCOL")
        .env_remove("GIT_PROTOCOL_FROM_USER")
        .env_remove("GIT_TEST_ASSUME_DIFFERENT_OWNER");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().expect("run binary")
}

fn ok(f: &Fixture, dir: &Path, args: &[&str]) -> Output {
    let o = run(f, dir, args);
    assert!(o.status.success(), "git {args:?}: {}", err(&o));
    o
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

/// The exact stderr stock git 2.55.0 writes for a repository it will not touch.
/// `sq_quote_buf_pretty()` leaves a path of ordinary characters unquoted, so the
/// two occurrences differ only in the surrounding punctuation.
fn dubious(path: &Path) -> String {
    let p = path.display();
    format!(
        "fatal: detected dubious ownership in repository at '{p}'\n\
         To add an exception for this directory, call:\n\
         \n\
         \tgit config --global --add safe.directory {p}\n"
    )
}

// ---------------------------------------------------------------------------
// (1) safe.directory / dubious ownership
// ---------------------------------------------------------------------------

/// The refusal itself, and that it names the *work tree* however deep the command
/// was run — `setup_git_directory_gently_1()` has already trimmed `dir` back to the
/// top by the time it reports, which is what makes the copy-and-paste line correct
/// from a subdirectory too.
#[test]
fn dubious_ownership_refuses_and_names_the_work_tree() {
    let f = fixture("own");
    let assume = [("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1")];
    let want = dubious(&f.work);

    for dir in [&f.work, &f.work.join("sub")] {
        let o = run_env(&f, dir, &assume, &["status", "--short"]);
        assert_eq!(err(&o), want, "from {}", dir.display());
        assert_eq!(code(&o), 128, "from {}", dir.display());
        assert_eq!(out(&o), "");
    }
}

/// Which commands the gate applies to. `RUN_SETUP` dies; `RUN_SETUP_GENTLY` and
/// the no-setup commands carry on with `*nongit_ok = 1`. Both halves are asserted
/// because a gate that fires for `git version` would be unusable and a gate that
/// skips `git log` would be pointless.
#[test]
fn dubious_ownership_applies_to_the_commands_that_need_a_repository() {
    let f = fixture("own-verbs");
    let assume = [("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1")];
    let want = dubious(&f.work);

    for args in [
        &["status", "--short"][..],
        &["log", "--oneline"],
        &["branch"],
        &["rev-parse", "HEAD"],
        &["cat-file", "-p", "HEAD"],
        &["ls-files"],
        &["tag"],
    ] {
        let o = run_env(&f, &f.work, &assume, args);
        assert_eq!(err(&o), want, "git {args:?}");
        assert_eq!(code(&o), 128, "git {args:?}");
    }

    // The gentle and no-setup side: none of these needs the repository, so none of
    // them refuses. Their exit codes are their own business; what is asserted is
    // that the ownership message is absent.
    for args in [
        &["version"][..],
        &["help"],
        &["config", "--list"],
        &["hash-object", "a.txt"],
    ] {
        let o = run_env(&f, &f.work, &assume, args);
        assert!(
            !err(&o).contains("dubious ownership"),
            "git {args:?} should not consult ownership: {}",
            err(&o)
        );
    }
}

/// `$GIT_DIR` returns `GIT_DIR_EXPLICIT` at setup.c:1560-1564, before the discovery
/// loop that holds the check — naming the repository outright is consent.
#[test]
fn dubious_ownership_is_skipped_for_an_explicit_git_dir() {
    let f = fixture("own-gitdir");
    let git_dir = f.work.join(".git");
    let o = run_env(
        &f,
        &f.work,
        &[
            ("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1"),
            ("GIT_DIR", git_dir.to_str().unwrap()),
        ],
        &["status", "--short"],
    );
    assert_eq!(err(&o), "");
    assert_eq!(code(&o), 0);
}

/// The exemptions, one per branch of `safe_directory_cb()` (setup.c:1337-1395).
#[test]
fn safe_directory_exemptions_match_setup_c() {
    let f = fixture("own-safe");
    let work = f.work.to_str().unwrap().to_owned();
    let root = f.root.to_str().unwrap().to_owned();
    let assume = [("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1")];

    let accepts = |args: Vec<String>, why: &str| {
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        let o = run_env(&f, &f.work, &assume, &argv);
        assert_eq!(err(&o), "", "{why}");
        assert_eq!(code(&o), 0, "{why}");
    };
    let refuses = |args: Vec<String>, why: &str| {
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        let o = run_env(&f, &f.work, &assume, &argv);
        assert_eq!(err(&o), dubious(&f.work), "{why}");
        assert_eq!(code(&o), 128, "{why}");
    };
    let with = |values: &[&str]| -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        for value in values {
            v.push("-c".into());
            v.push(format!("safe.directory={value}"));
        }
        v.extend(["status".to_string(), "--short".to_string()]);
        v
    };

    // The exact path, and the literal `*` that means "every repository".
    accepts(with(&[&work]), "the work tree itself");
    accepts(with(&["*"]), "the wildcard value");
    // `.` is the one relative value git accepts: it normalizes to the current
    // directory, which is the top of this work tree.
    accepts(with(&["."]), "`.` at the top of the work tree");
    // `/*` is a prefix match over what is *below* a directory. `fspathncmp` compares
    // `len - 1` bytes, so the trailing `*` is dropped and the `/` is not — the
    // parent's glob covers the repository, the repository's own does not cover
    // itself.
    accepts(with(&[&format!("{root}/*")]), "the parent's `/*` form");
    refuses(
        with(&[&format!("{work}/*")]),
        "`<work>/*` must not match `<work>`",
    );
    // A path that is not there is skipped without a warning: a `~/.gitconfig` is
    // shared across machines and an entry naming a repository on another one is not
    // an error here.
    refuses(
        with(&["/definitely/not/here"]),
        "a missing entry exempts nothing",
    );

    // The scan does not stop at a match — `is_safe` is rewritten by every entry, so
    // an empty value *resets* it and the last word wins.
    refuses(
        with(&[&work, ""]),
        "an empty value resets an earlier exemption",
    );
    accepts(with(&["", &work]), "and only what precedes it");

    // A relative entry other than `.` is refused with a warning and does not exempt.
    let o = run_env(
        &f,
        &f.work,
        &assume,
        &["-c", "safe.directory=relative/path", "status", "--short"],
    );
    assert_eq!(
        err(&o),
        format!(
            "warning: safe.directory 'relative/path' not absolute\n{}",
            dubious(&f.work)
        )
    );
    assert_eq!(code(&o), 128);
}

/// The exemption is read from *protected* configuration only
/// (`git_protected_config`, config.c:2447-2465 → `ignore_repo = 1`), so a
/// repository cannot whitelist itself in its own `config`. Without this a dropped-in
/// repository would carry its own permission slip.
#[test]
fn safe_directory_is_not_read_from_the_repository_itself() {
    let f = fixture("own-local");
    ok(
        &f,
        &f.work,
        &[
            "config",
            "--local",
            "safe.directory",
            f.work.to_str().unwrap(),
        ],
    );
    let o = run_env(
        &f,
        &f.work,
        &[("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1")],
        &["status", "--short"],
    );
    assert_eq!(err(&o), dubious(&f.work));
    assert_eq!(code(&o), 128);
}

/// A bare repository is identified by its git directory, not by a work tree it does
/// not have — so that is the path the message names and the path an exemption has to
/// be written against.
#[test]
fn dubious_ownership_of_a_bare_repository_names_the_git_directory() {
    let f = fixture("own-bare");
    let bare = f.root.join("bare.git");
    ok(
        &f,
        &f.root,
        &["init", "-q", "--bare", bare.to_str().unwrap()],
    );

    let o = run_env(
        &f,
        &bare,
        &[("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1")],
        &["rev-parse", "--git-dir"],
    );
    assert_eq!(err(&o), dubious(&bare));
    assert_eq!(code(&o), 128);

    let o = run_env(
        &f,
        &bare,
        &[("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1")],
        &[
            "-c",
            &format!("safe.directory={}", bare.display()),
            "rev-parse",
            "--git-dir",
        ],
    );
    assert_eq!(err(&o), "");
    assert_eq!(out(&o), ".\n");
}

/// `sq_quote_buf_pretty()` (quote.c:50-70) quotes only when the text needs it, so a
/// path with a space comes back single-quoted in the copy-and-paste line while the
/// first occurrence — which git prints inside its own `'…'` — does not.
#[test]
fn the_exception_line_is_shell_quoted_only_when_it_has_to_be() {
    let f = fixture("own-space");
    let spaced = f.root.join("sp ace");
    std::fs::create_dir_all(&spaced).unwrap();
    ok(&f, &spaced, &["init", "-q", "-b", "main"]);

    let o = run_env(
        &f,
        &spaced,
        &[("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1")],
        &["status", "--short"],
    );
    let p = spaced.display();
    assert_eq!(
        err(&o),
        format!(
            "fatal: detected dubious ownership in repository at '{p}'\n\
             To add an exception for this directory, call:\n\
             \n\
             \tgit config --global --add safe.directory '{p}'\n"
        )
    );
    assert_eq!(code(&o), 128);
}

/// Nothing fires when the repository really is ours, which is every ordinary run.
#[test]
fn an_owned_repository_is_never_questioned() {
    let f = fixture("own-clean");
    for args in [
        &["status", "--short"][..],
        &["log", "--oneline"],
        &["rev-parse", "HEAD"],
    ] {
        let o = run(&f, &f.work, args);
        assert!(!err(&o).contains("dubious"), "git {args:?}: {}", err(&o));
        assert_eq!(code(&o), 0, "git {args:?}: {}", err(&o));
    }
}

// ---------------------------------------------------------------------------
// (2) protocol.<name>.allow
// ---------------------------------------------------------------------------

/// `GIT_PROTOCOL_FROM_USER=0` is what git sets around everything it runs on the
/// user's behalf rather than at their request. `file` is not in
/// `get_protocol_config()`'s known-safe list (transport.c:1110-1114), so it falls to
/// `PROTOCOL_ALLOW_USER_ONLY` and is refused under it.
#[test]
fn a_local_url_is_refused_when_it_did_not_come_from_the_user() {
    let f = fixture("proto");
    let not_user = [("GIT_PROTOCOL_FROM_USER", "0")];

    let o = run_env(&f, &f.work, &not_user, &["ls-remote", "."]);
    assert_eq!(err(&o), "fatal: transport 'file' not allowed\n");
    assert_eq!(code(&o), 128);
    assert_eq!(
        out(&o),
        "",
        "no refs are listed once the transport is refused"
    );

    // The same URL spelled as a scheme reaches the same policy.
    let url = format!("file://{}", f.work.display());
    let o = run_env(&f, &f.work, &not_user, &["ls-remote", &url]);
    assert_eq!(err(&o), "fatal: transport 'file' not allowed\n");
    assert_eq!(code(&o), 128);

    // And with the variable absent — the default `from_user` is 1 — it is allowed.
    let o = run(&f, &f.work, &["ls-remote", "."]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(out(&o).contains("refs/heads/main"), "{}", out(&o));
}

/// Each of the three ways to widen the policy, and the one that narrows it.
#[test]
fn the_policy_can_be_widened_and_narrowed() {
    let f = fixture("proto-cfg");
    let listed = |o: &Output| out(o).contains("refs/heads/main");

    // `$GIT_ALLOW_PROTOCOL` is an override, not a cascade: when it is set the
    // configuration is not consulted at all.
    let o = run_env(
        &f,
        &f.work,
        &[
            ("GIT_PROTOCOL_FROM_USER", "0"),
            ("GIT_ALLOW_PROTOCOL", "file"),
        ],
        &["ls-remote", "."],
    );
    assert!(listed(&o) && code(&o) == 0, "{}{}", out(&o), err(&o));

    // …which is why listing only *another* scheme refuses `file` even though the
    // built-in default would have allowed it to a user typing the command.
    let o = run_env(
        &f,
        &f.work,
        &[("GIT_ALLOW_PROTOCOL", "ssh")],
        &["ls-remote", "."],
    );
    assert_eq!(err(&o), "fatal: transport 'file' not allowed\n");
    assert_eq!(code(&o), 128);

    // An empty list is still a list, and allows nothing.
    let o = run_env(
        &f,
        &f.work,
        &[("GIT_ALLOW_PROTOCOL", "")],
        &["ls-remote", "."],
    );
    assert_eq!(err(&o), "fatal: transport 'file' not allowed\n");
    assert_eq!(code(&o), 128);

    // `protocol.<name>.allow` and the `protocol.allow` fallback, both from the
    // full configuration cascade.
    for key in ["protocol.file.allow", "protocol.allow"] {
        let o = run_env(
            &f,
            &f.work,
            &[("GIT_PROTOCOL_FROM_USER", "0")],
            &["-c", &format!("{key}=always"), "ls-remote", "."],
        );
        assert!(listed(&o) && code(&o) == 0, "{key}: {}{}", out(&o), err(&o));
    }

    // `never` refuses even a URL the user typed.
    let o = run(
        &f,
        &f.work,
        &["-c", "protocol.file.allow=never", "ls-remote", "."],
    );
    assert_eq!(err(&o), "fatal: transport 'file' not allowed\n");
    assert_eq!(code(&o), 128);

    // A value outside the three words is `die()`, not a fallback.
    let o = run(
        &f,
        &f.work,
        &["-c", "protocol.file.allow=bogus", "ls-remote", "."],
    );
    assert_eq!(
        err(&o),
        "fatal: unknown value for config 'protocol.file.allow': bogus\n"
    );
    assert_eq!(code(&o), 128);
}

/// The gate is on the *connection*, not on the command: `--get-url` never opens one
/// and is unaffected, while `clone` prints its banner first and refuses after —
/// `transport_get()` is reached inside `cmd_clone`, not before it.
#[test]
fn the_transport_gate_fires_where_the_connection_is_opened() {
    let f = fixture("proto-order");
    let not_user = [("GIT_PROTOCOL_FROM_USER", "0")];

    let o = run_env(&f, &f.work, &not_user, &["ls-remote", "--get-url", "."]);
    assert_eq!(out(&o), ".\n");
    assert_eq!(err(&o), "");
    assert_eq!(code(&o), 0);

    let dst = f.root.join("cloned");
    let o = run_env(
        &f,
        &f.root,
        &not_user,
        &["clone", f.work.to_str().unwrap(), dst.to_str().unwrap()],
    );
    assert_eq!(
        err(&o),
        format!(
            "Cloning into '{}'...\nfatal: transport 'file' not allowed\n",
            dst.display()
        )
    );
    assert_eq!(code(&o), 128);
}

/// `fetch` and `push` reach the same `git_connect()`, so both are gated. `fetch`
/// is the one that matters: it is what a submodule update runs, and git clears
/// `$GIT_PROTOCOL_FROM_USER` around exactly that.
#[test]
fn fetch_and_push_are_gated_too() {
    let f = fixture("proto-fetch");
    let other = f.root.join("other");
    ok(
        &f,
        &f.root,
        &[
            "clone",
            "-q",
            f.work.to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    let not_user = [("GIT_PROTOCOL_FROM_USER", "0")];

    for args in [&["fetch", "origin"][..], &["push", "origin", "main"]] {
        let o = run_env(&f, &other, &not_user, args);
        assert_eq!(
            err(&o),
            "fatal: transport 'file' not allowed\n",
            "git {args:?}"
        );
        assert_eq!(code(&o), 128, "git {args:?}");
    }

    // …and both work when the policy allows the scheme.
    let o = run_env(
        &f,
        &other,
        &[
            ("GIT_PROTOCOL_FROM_USER", "0"),
            ("GIT_ALLOW_PROTOCOL", "file"),
        ],
        &["fetch", "origin"],
    );
    assert_eq!(code(&o), 0, "{}", err(&o));
}

// ---------------------------------------------------------------------------
// (3) GIT_CONFIG_GLOBAL
// ---------------------------------------------------------------------------

/// `--global` names ONE file (`git_global_config()`, config.c:1505-1523) and
/// `cmd_config_list()` dies when it cannot be read (builtin/config.c:1060-1068).
/// Silently listing nothing made a broken `$GIT_CONFIG_GLOBAL` look like an empty
/// configuration — the caller could not tell "no settings" from "wrong path".
#[test]
fn a_global_config_that_cannot_be_read_is_fatal_for_list() {
    let f = fixture("cfgglobal");
    let missing = f.root.join("no-such-config");

    let o = run_env(
        &f,
        &f.work,
        &[("GIT_CONFIG_GLOBAL", missing.to_str().unwrap())],
        &["config", "--list", "--global"],
    );
    assert_eq!(
        err(&o),
        format!(
            "fatal: unable to read config file '{}': No such file or directory\n",
            missing.display()
        )
    );
    assert_eq!(code(&o), 128);
    assert_eq!(out(&o), "");

    // The get forms are unaffected — git splits those two paths, and a missing key
    // is exit 1 with nothing on stderr whatever the reason it is missing.
    let o = run_env(
        &f,
        &f.work,
        &[("GIT_CONFIG_GLOBAL", missing.to_str().unwrap())],
        &["config", "--global", "user.name"],
    );
    assert_eq!(err(&o), "");
    assert_eq!(code(&o), 1);

    // A file that exists reads normally, which is the accepting half of the gate.
    let present = f.root.join("a-config");
    std::fs::write(&present, "[user]\n\tname = someone\n").unwrap();
    let o = run_env(
        &f,
        &f.work,
        &[("GIT_CONFIG_GLOBAL", present.to_str().unwrap())],
        &["config", "--list", "--global"],
    );
    assert_eq!(err(&o), "");
    assert_eq!(out(&o), "user.name=someone\n");
    assert_eq!(code(&o), 0);
}

/// `$GIT_CONFIG_SYSTEM` is the same story through `git_system_config()`
/// (config.c:1496-1503), and `GIT_CONFIG_NOSYSTEM` does not suppress it: that is
/// checked by `git_config_system()` in the *cascade* (config.c:1540-1542), not by
/// the scope flag.
#[test]
fn a_system_config_that_cannot_be_read_is_fatal_for_list() {
    let f = fixture("cfgsystem");
    let missing = f.root.join("no-such-system");
    let o = run_env(
        &f,
        &f.work,
        &[("GIT_CONFIG_SYSTEM", missing.to_str().unwrap())],
        &["config", "--list", "--system"],
    );
    assert_eq!(
        err(&o),
        format!(
            "fatal: unable to read config file '{}': No such file or directory\n",
            missing.display()
        )
    );
    assert_eq!(code(&o), 128);
}

/// The XDG file is `git_global_config()`'s *fallback*, not a peer: `~/.gitconfig`
/// wins whenever it is readable and the XDG file is read only when it is not.
/// Merging the pair showed entries under `--global --list` that stock git does not.
#[test]
fn global_list_reads_one_file_not_the_xdg_pair() {
    let f = fixture("cfgxdg");
    let xdg = f.home.join(".config/git");
    std::fs::create_dir_all(&xdg).unwrap();
    std::fs::write(xdg.join("config"), "[xdg]\n\tk = v\n").unwrap();

    // With no `~/.gitconfig`, the XDG file is the one picked.
    let o = run(&f, &f.work, &["config", "--list", "--global"]);
    assert_eq!(out(&o), "xdg.k=v\n");
    assert_eq!(code(&o), 0);

    // With one, it wins outright and the XDG file is not read at all.
    std::fs::write(f.home.join(".gitconfig"), "[home]\n\tk = v\n").unwrap();
    let o = run(&f, &f.work, &["config", "--list", "--global"]);
    assert_eq!(out(&o), "home.k=v\n");
    assert_eq!(code(&o), 0);
}

// ---------------------------------------------------------------------------
// (4) GIT_ALTERNATE_OBJECT_DIRECTORIES
// ---------------------------------------------------------------------------

/// `odb_is_source_usable()` (odb.c:59-73) names an alternate that has gone missing
/// and the command carries on — it is an `error()`, not a `die()`. Silence here
/// meant a repository whose alternates had moved looked healthy right up until an
/// object could not be found.
#[test]
fn a_missing_alternate_is_reported_and_the_command_continues() {
    let f = fixture("alt");
    let missing = f.root.join("no-such-objects");
    let line = format!(
        "error: object directory {} does not exist; check .git/objects/info/alternates\n",
        missing.display()
    );

    let o = run_env(
        &f,
        &f.work,
        &[(
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            missing.to_str().unwrap(),
        )],
        &["log", "--oneline"],
    );
    assert_eq!(err(&o), line);
    assert_eq!(code(&o), 0, "the entry is dropped, not fatal");
    assert!(out(&o).contains("one"), "the log still prints: {}", out(&o));

    // Every missing entry is named, in the order it was listed.
    let second = f.root.join("also-not-here");
    let o = run_env(
        &f,
        &f.work,
        &[(
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            &format!("{}:{}", missing.display(), second.display()),
        )],
        &["log", "--oneline"],
    );
    assert_eq!(
        err(&o),
        format!(
            "{line}error: object directory {} does not exist; check .git/objects/info/alternates\n",
            second.display()
        )
    );

    // `is_directory()` is a stat, so a regular file is "does not exist" too, and a
    // relative entry is resolved against the current directory before it is named.
    let o = run_env(
        &f,
        &f.work,
        &[("GIT_ALTERNATE_OBJECT_DIRECTORIES", "a.txt")],
        &["log", "--oneline"],
    );
    assert_eq!(
        err(&o),
        format!(
            "error: object directory {} does not exist; check .git/objects/info/alternates\n",
            f.work.join("a.txt").display()
        )
    );

    // An entry that is there says nothing, and neither does an empty variable.
    for value in [f.work.join(".git/objects").to_str().unwrap(), ""] {
        let o = run_env(
            &f,
            &f.work,
            &[("GIT_ALTERNATE_OBJECT_DIRECTORIES", value)],
            &["log", "--oneline"],
        );
        assert_eq!(err(&o), "", "value {value:?}");
        assert_eq!(code(&o), 0, "value {value:?}");
    }
}

// ---------------------------------------------------------------------------
// (5) GIT_OBJECT_DIRECTORY
// ---------------------------------------------------------------------------

/// `is_git_directory()` (setup.c:433-442) tests `$GIT_OBJECT_DIRECTORY` *in place
/// of* `<gitdir>/objects`, so an unreachable one un-recognises every candidate on
/// the way up and discovery ends at the ceiling. There is no second message naming
/// the variable; the repository simply is not found.
#[test]
fn an_unreachable_object_directory_hides_the_repository() {
    let f = fixture("objdir");
    let missing = f.work.join(".git/no-such-objects");
    const NOT_A_REPO: &str =
        "fatal: not a git repository (or any of the parent directories): .git\n";

    for args in [
        &["status"][..],
        &["log", "--oneline"],
        &["rev-parse", "--git-dir"],
    ] {
        let o = run_env(
            &f,
            &f.work,
            &[("GIT_OBJECT_DIRECTORY", missing.to_str().unwrap())],
            args,
        );
        assert_eq!(err(&o), NOT_A_REPO, "git {args:?}");
        assert_eq!(code(&o), 128, "git {args:?}");
    }

    // It is `access(X_OK)`, not "is a directory": a regular file fails it, and so
    // does an empty value (`getenv` returns a pointer, so the variable counts as
    // set and `access("")` fails).
    for value in [f.work.join("a.txt").to_str().unwrap(), ""] {
        let o = run_env(&f, &f.work, &[("GIT_OBJECT_DIRECTORY", value)], &["status"]);
        assert_eq!(err(&o), NOT_A_REPO, "value {value:?}");
        assert_eq!(code(&o), 128, "value {value:?}");
    }

    // A directory that exists passes, which is the accepting half — setup succeeds
    // and the command runs.
    let o = run_env(
        &f,
        &f.work,
        &[(
            "GIT_OBJECT_DIRECTORY",
            f.work.join(".git/objects").to_str().unwrap(),
        )],
        &["status", "--short"],
    );
    assert_eq!(err(&o), "");
    assert_eq!(code(&o), 0);

    // Naming the repository outright reports the directory it was handed rather
    // than the walk that never happened (setup.c:1127-1133).
    let git_dir = f.work.join(".git");
    let o = run_env(
        &f,
        &f.work,
        &[
            ("GIT_OBJECT_DIRECTORY", missing.to_str().unwrap()),
            ("GIT_DIR", git_dir.to_str().unwrap()),
        ],
        &["status"],
    );
    assert_eq!(
        err(&o),
        format!("fatal: not a git repository: '{}'\n", git_dir.display())
    );
    assert_eq!(code(&o), 128);
}

/// Nothing has read configuration when `is_git_directory()` runs, which fixes the
/// order between this gate and the command-line config one: the missing repository
/// is reported even though `$GIT_CONFIG_COUNT` is also malformed.
#[test]
fn the_object_directory_gate_precedes_the_config_gate() {
    let f = fixture("objdir-order");
    let o = run_env(
        &f,
        &f.work,
        &[
            (
                "GIT_OBJECT_DIRECTORY",
                f.work.join(".git/nope").to_str().unwrap(),
            ),
            ("GIT_CONFIG_COUNT", "bogus"),
        ],
        &["status"],
    );
    assert_eq!(
        err(&o),
        "fatal: not a git repository (or any of the parent directories): .git\n"
    );
    assert_eq!(code(&o), 128);
}

// ---------------------------------------------------------------------------
// (6) GIT_CONFIG_COUNT / GIT_CONFIG_KEY_<n> / GIT_CONFIG_VALUE_<n>
// ---------------------------------------------------------------------------

/// `git_config_from_parameters()` (config.c:731-780) reports what was wrong, and
/// its caller (config.c:1601-1602) dies. Two lines and exit 128 — this port used to
/// print gitoxide's own sentence through the `zvcs: <verb>:` channel at exit 1,
/// which a caller testing for 128 read as success.
#[test]
fn a_malformed_command_line_config_environment_is_fatal() {
    let f = fixture("cfgcount");
    const DIE: &str = "fatal: unable to parse command-line config\n";

    let cases: &[(&[(&str, &str)], &str)] = &[
        // `strtoul` leaves `endp` on the junk.
        (
            &[("GIT_CONFIG_COUNT", "bogus")],
            "error: bogus count in GIT_CONFIG_COUNT\n",
        ),
        (
            &[("GIT_CONFIG_COUNT", "1x")],
            "error: bogus count in GIT_CONFIG_COUNT\n",
        ),
        // …and wraps a negative, which then trips the `> INT_MAX` arm instead.
        (
            &[("GIT_CONFIG_COUNT", "-1")],
            "error: too many entries in GIT_CONFIG_COUNT\n",
        ),
        (
            &[("GIT_CONFIG_COUNT", "99999999999999999999999")],
            "error: too many entries in GIT_CONFIG_COUNT\n",
        ),
        // `strtoul` skips leading whitespace, so this is one override and the
        // failure that follows is the missing key, not a bogus count.
        (
            &[("GIT_CONFIG_COUNT", " 1")],
            "error: missing config key GIT_CONFIG_KEY_0\n",
        ),
        (
            &[("GIT_CONFIG_COUNT", "3")],
            "error: missing config key GIT_CONFIG_KEY_0\n",
        ),
        (
            &[("GIT_CONFIG_COUNT", "1"), ("GIT_CONFIG_KEY_0", "a.b")],
            "error: missing config value GIT_CONFIG_VALUE_0\n",
        ),
        (
            &[
                ("GIT_CONFIG_COUNT", "1"),
                ("GIT_CONFIG_KEY_0", "nosection"),
                ("GIT_CONFIG_VALUE_0", "x"),
            ],
            "error: key does not contain a section: nosection\n",
        ),
    ];

    for (env, want) in cases {
        let o = run_env(&f, &f.work, env, &["status", "--short"]);
        assert_eq!(err(&o), format!("{want}{DIE}"), "{env:?}");
        assert_eq!(code(&o), 128, "{env:?}");
    }

    // An empty value is zero overrides, not an error: `strtoul("")` is 0 with
    // `endp` at the terminator.
    let o = run_env(
        &f,
        &f.work,
        &[("GIT_CONFIG_COUNT", "")],
        &["status", "--short"],
    );
    assert_eq!(err(&o), "");
    assert_eq!(code(&o), 0);

    // A well-formed pair is applied, which is the accepting half.
    let o = run_env(
        &f,
        &f.work,
        &[
            ("GIT_CONFIG_COUNT", "1"),
            ("GIT_CONFIG_KEY_0", "user.name"),
            ("GIT_CONFIG_VALUE_0", "someone"),
        ],
        &["config", "user.name"],
    );
    assert_eq!(out(&o), "someone\n");
    assert_eq!(code(&o), 0);
}

/// Only the commands that never read configuration escape it, and `git version` is
/// the one a wrapper is most likely to call first — a gate that broke it would
/// break version probing under any environment carrying a stale override.
#[test]
fn the_config_gate_skips_only_the_commands_that_read_no_config() {
    let f = fixture("cfgcount-verbs");
    let bogus = [("GIT_CONFIG_COUNT", "bogus")];

    for args in [&["version"][..], &["help"], &["stripspace"]] {
        let o = run_env(&f, &f.work, &bogus, args);
        assert!(
            !err(&o).contains("GIT_CONFIG_COUNT"),
            "git {args:?} reads no config: {}",
            err(&o)
        );
    }

    for args in [
        &["status"][..],
        &["config", "--list"],
        &["init"],
        &["ls-remote", "."],
    ] {
        let o = run_env(&f, &f.work, &bogus, args);
        assert_eq!(
            err(&o),
            "error: bogus count in GIT_CONFIG_COUNT\nfatal: unable to parse command-line config\n",
            "git {args:?}"
        );
        assert_eq!(code(&o), 128, "git {args:?}");
    }
}

/// The first read of configuration is the one `get_allowed_bare_repo()` and
/// `ensure_valid_ownership()` make, so a bad override is reported before either
/// policy refusal — the caller is told what is actually broken.
#[test]
fn the_config_gate_precedes_the_ownership_gate() {
    let f = fixture("cfgcount-order");
    let o = run_env(
        &f,
        &f.work,
        &[
            ("GIT_CONFIG_COUNT", "bogus"),
            ("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1"),
        ],
        &["status"],
    );
    assert_eq!(
        err(&o),
        "error: bogus count in GIT_CONFIG_COUNT\nfatal: unable to parse command-line config\n"
    );
    assert_eq!(code(&o), 128);
}
