//! `git hook run` when a hook never starts.
//!
//! `start_command()` has two ways to fail and git spells them differently.
//! `prepare_cmd()` (run-command.c:435-444) does the `PATH` lookup itself when the
//! program word holds no directory separator and returns -1 *before* the fork with
//! `errno` forced to `ENOENT`; the parent reports that as
//! `error: cannot run <cmd>: <strerror>` (run-command.c:757-762). Anything else
//! fails inside the forked child, whose `errno` comes back up the notify pipe and
//! is printed as `fatal: cannot exec '<cmd>': <strerror>` — `fatal:` because
//! `child_err_spew()` installs the *die* message routine for the duration
//! (run-command.c:384, 403-405).
//!
//! Both name `cmd->args.v[0]`, the command as configured, never the resolved path
//! and never the `SHELL_PATH` wrapper `prepare_shell_cmd()` may have put in front
//! of it. And either way `notify_start_failure()` (hook.c:638-647) ORs 1 into the
//! result and returns 1, which sets `pp->shutdown` (run-command.c:1679-1680) —
//! so every hook queued behind the broken one is abandoned, including the
//! traditional `.git/hooks/<event>` script that always runs last.
//!
//! Measured against git 2.55.0; the expectations below are stock's bytes.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn repo(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-hookstart-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    assert!(
        Command::new(BIN)
            .args(["init", "-q", "-b", "main"])
            .current_dir(&root)
            .status()
            .unwrap()
            .success(),
        "init failed"
    );
    root
}

/// Run `git hook run <event>` with the given `-c` overrides, isolated from any
/// ambient configuration so a developer's `~/.gitconfig` cannot add a hook.
fn hook_run(dir: &Path, overrides: &[&str], event: &str) -> Output {
    let mut cmd = Command::new(BIN);
    for over in overrides {
        cmd.args(["-c", over]);
    }
    cmd.args(["hook", "run", event])
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", dir)
        .env("ZVCS_HOME", dir)
        .env("GIT_CONFIG_GLOBAL", dir.join("no-such-global"))
        .output()
        .unwrap()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Write an executable `.git/hooks/<event>` that announces itself on stdout.
/// `git hook run` points hook stdout at stderr, so its marker lands there.
fn hookdir_script(dir: &Path, event: &str, marker: &str) {
    let hooks = dir.join(".git/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let path = hooks.join(event);
    std::fs::write(&path, format!("#!/bin/sh\necho {marker}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// An absolute command that does not exist takes the in-child exec failure:
/// `fatal: cannot exec '<cmd>': No such file or directory`, exit 1. A slash-free
/// name nothing on `PATH` answers takes the pre-fork branch instead and reads
/// `error: cannot run <cmd>: ...` — no quotes, `error:` not `fatal:`.
///
/// The two spellings are not interchangeable: `prepare_cmd()` only reaches the
/// PATH scan for a word with no directory separator, so which message a command
/// gets is decided by its spelling, not by why it is unusable.
#[test]
fn unstartable_hook_reports_gits_two_distinct_diagnostics() {
    let dir = repo("spellings");

    let absolute = hook_run(
        &dir,
        &["hook.a.command=/zvcs-no-such-dir/nope", "hook.a.event=pre-commit"],
        "pre-commit",
    );
    assert_eq!(absolute.status.code(), Some(1), "{absolute:?}");
    assert_eq!(
        stderr(&absolute),
        "fatal: cannot exec '/zvcs-no-such-dir/nope': No such file or directory\n"
    );

    let bare = hook_run(
        &dir,
        &["hook.b.command=zvcs-no-such-program-xyz", "hook.b.event=pre-commit"],
        "pre-commit",
    );
    assert_eq!(bare.status.code(), Some(1), "{bare:?}");
    assert_eq!(
        stderr(&bare),
        "error: cannot run zvcs-no-such-program-xyz: No such file or directory\n"
    );
}

/// A command word carrying a shell metacharacter is wrapped in `<SHELL_PATH> -c`
/// by `prepare_shell_cmd()` (run-command.c:293-305), so `out->v[1]` is the shell
/// — which exists. The spawn therefore succeeds and the *shell* reports the
/// missing program, exiting 127. git's own exec diagnostics must not appear.
#[test]
fn a_shell_wrapped_hook_fails_in_the_shell_not_in_the_spawn() {
    let dir = repo("shellwrap");
    let out = hook_run(
        &dir,
        &[
            "hook.a.command=/zvcs-no-such-dir/nope && true",
            "hook.a.event=pre-commit",
        ],
        "pre-commit",
    );
    assert_eq!(out.status.code(), Some(127), "{out:?}");
    let err = stderr(&out);
    assert!(
        !err.contains("cannot exec") && !err.contains("cannot run"),
        "the spawn succeeded, so git reports nothing about it: {err:?}"
    );
}

/// A start failure shuts the whole run down. Both the configured hook queued
/// after the broken one and the traditional `.git/hooks/pre-commit` script — which
/// `hook.c` always appends last — must stay unrun, and the exit code is 1 from
/// `hook_cb->rc |= 1` rather than anything the later hooks would have produced.
#[test]
fn a_start_failure_abandons_every_later_hook() {
    let dir = repo("shutdown");
    hookdir_script(&dir, "pre-commit", "HOOKDIR-RAN");

    let out = hook_run(
        &dir,
        &[
            "hook.a.command=/zvcs-no-such-dir/nope",
            "hook.a.event=pre-commit",
            "hook.b.command=echo LATER-RAN",
            "hook.b.event=pre-commit",
        ],
        "pre-commit",
    );
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let err = stderr(&out);
    assert_eq!(
        err, "fatal: cannot exec '/zvcs-no-such-dir/nope': No such file or directory\n",
        "only the failing hook says anything"
    );
    assert!(!err.contains("LATER-RAN"), "later configured hook ran: {err:?}");
    assert!(!err.contains("HOOKDIR-RAN"), "hookdir hook ran: {err:?}");
}

/// The shutdown is triggered by the failure, not by position: a healthy hook in
/// front of the broken one still runs and still contributes its output, and only
/// what comes *after* is abandoned.
#[test]
fn hooks_ahead_of_the_failure_still_run() {
    let dir = repo("ordering");
    hookdir_script(&dir, "pre-commit", "HOOKDIR-RAN");

    let out = hook_run(
        &dir,
        &[
            "hook.a.command=echo FIRST-RAN",
            "hook.a.event=pre-commit",
            "hook.b.command=/zvcs-no-such-dir/nope",
            "hook.b.event=pre-commit",
        ],
        "pre-commit",
    );
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let err = stderr(&out);
    assert!(err.contains("FIRST-RAN"), "hook ahead of the failure was skipped: {err:?}");
    assert!(
        err.contains("fatal: cannot exec '/zvcs-no-such-dir/nope': No such file or directory"),
        "{err:?}"
    );
    assert!(!err.contains("HOOKDIR-RAN"), "hookdir hook ran after the failure: {err:?}");
}

/// A directory and a non-executable regular file both exec with `EACCES`, so they
/// take the in-child branch and report `Permission denied` — the spawn's own
/// errno, not the `ENOENT` `prepare_cmd()` substitutes on its pre-fork path.
#[test]
fn an_unexecutable_target_reports_its_own_errno() {
    let dir = repo("eacces");
    let plain = dir.join("not-executable");
    std::fs::write(&plain, "data\n").unwrap();

    for target in [dir.join(".git"), plain] {
        let spec = format!("hook.a.command={}", target.display());
        let out = hook_run(&dir, &[&spec, "hook.a.event=pre-commit"], "pre-commit");
        assert_eq!(out.status.code(), Some(1), "{out:?}");
        assert_eq!(
            stderr(&out),
            format!("fatal: cannot exec '{}': Permission denied\n", target.display())
        );
    }
}
