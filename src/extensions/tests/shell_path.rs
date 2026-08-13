//! The shell this binary spawns is git's `SHELL_PATH` — one absolute path — and
//! never a `sh` resolved on `PATH`.
//!
//! git fixes `SHELL_PATH` at compile time (`/bin/sh`) and reaches it through
//! `git_shell_path()`, so a user whose `PATH` front-loads a different `sh` still
//! gets the same interpreter for hooks, `!`-aliases, filters, the pager,
//! mergetool/difftool, `filter-branch`, `submodule foreach` and `rebase --exec`.
//! Every test here runs the binary with a **hostile `sh` first on `PATH`**: a
//! shim that touches a marker file, prints a distinctive string and exits 7. If
//! any shell child were PATH-resolved, the marker would appear and the command's
//! output would be the shim's.
//!
//! The direct-exec test is the other half, and keeps the rest from being
//! vacuous: `prepare_shell_cmd()` leaves a metacharacter-free command word alone
//! and git then PATH-resolves *it*. A probe program planted in the same shadow
//! directory must be found — proving the poisoned `PATH` really is in force for
//! the child, and that only the shell is exempt from it.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// What the shim prints when it is (wrongly) used as the shell.
const FAKE_OUTPUT: &str = "FAKE-SH-WAS-USED";

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
    home: PathBuf,
    shadow: PathBuf,
    marker: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// A hermetic repo, an isolated HOME, and a `PATH` directory holding a
    /// hostile `sh` plus a `zvcsprobe` program that only a genuine PATH lookup
    /// can find.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-shellpath-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();

        let shadow = root.join("shadow-bin");
        let marker = root.join("fake-sh-was-run");
        std::fs::create_dir_all(&shadow).unwrap();
        // The shim's own `#!` line names the real shell by absolute path, so it
        // cannot recurse into itself.
        write_exec(
            &shadow.join("sh"),
            &format!(
                "#!/bin/sh\n: > {marker}\nprintf {FAKE_OUTPUT}\nexit 7\n",
                marker = marker.display()
            ),
        );
        write_exec(&shadow.join("zvcsprobe"), "#!/bin/sh\nprintf FOUND-ON-PATH\n");

        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();

        let f = Fixture { root, repo, home, shadow, marker };
        f.git(&["init", "-q", "-b", "main"]);
        std::fs::write(f.repo.join("a.txt"), "hello\n").unwrap();
        f.git(&["add", "a.txt"]);
        f.git(&["commit", "-q", "-m", "first"]);
        f
    }

    /// Run the binary with an ordinary `PATH` — setup only, never the assertion.
    fn git(&self, args: &[&str]) {
        let out = self.spawn(args, "/usr/bin:/bin");
        assert!(
            out.status.success(),
            "setup `git {args:?}` failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Run the binary with the hostile `sh` first on `PATH`.
    fn git_shadowed(&self, args: &[&str]) -> Output {
        let shadowed = format!("{}:/usr/bin:/bin", self.shadow.display());
        self.spawn(args, &shadowed)
    }

    fn spawn(&self, args: &[&str], path: &str) -> Output {
        Command::new(BIN)
            .args(args)
            .current_dir(&self.repo)
            .env("PATH", path)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("ZVCS_HOME", &self.home)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@e.x")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@e.x")
            .output()
            .unwrap()
    }

    /// The shim leaves this behind the instant it runs, so its absence is proof
    /// no child ever reached it — including children whose output we discard.
    fn assert_shim_untouched(&self) {
        assert!(
            !self.marker.exists(),
            "a shell child was resolved through PATH and hit the shim at {}",
            self.shadow.join("sh").display()
        );
    }
}

fn write_exec(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A `!`-alias whose body carries a shell metacharacter is a `use_shell` child:
/// it must reach `SHELL_PATH`, not the `sh` sitting in front of it on `PATH`.
#[test]
fn shell_alias_ignores_a_shell_planted_on_path() {
    let f = Fixture::new("alias");
    let out = f.git_shadowed(&["-c", "alias.zzsh=!echo REAL-SHELL", "zzsh"]);

    assert!(
        out.status.success(),
        "shell alias failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(stdout_of(&out).trim(), "REAL-SHELL");
    assert!(!stdout_of(&out).contains(FAKE_OUTPUT));
    f.assert_shim_untouched();
}

/// The same alias mechanism, with a body that has no metacharacter:
/// `prepare_shell_cmd()` execs it directly and git PATH-resolves the program.
/// Finding the probe proves the poisoned `PATH` is genuinely in force for the
/// child — so the tests above are not passing merely because `PATH` was ignored.
#[test]
fn a_bare_command_word_is_still_resolved_on_path() {
    let f = Fixture::new("direct");
    let out = f.git_shadowed(&["-c", "alias.zzp=!zvcsprobe", "zzp"]);

    assert!(
        out.status.success(),
        "direct-exec alias failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(stdout_of(&out).trim(), "FOUND-ON-PATH");
    f.assert_shim_untouched();
}

/// `filter-branch` evaluates its filters in a shell of its own — git's
/// `#!@SHELL_PATH@` script. The rewrite must succeed with the shim in front.
#[test]
fn filter_branch_filters_run_under_the_absolute_shell() {
    let f = Fixture::new("fb");
    let out = f.git_shadowed(&[
        "filter-branch",
        "-f",
        "--msg-filter",
        "cat; echo REWRITTEN",
        "HEAD",
    ]);
    assert!(
        out.status.success(),
        "filter-branch failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let log = f.git_shadowed(&["log", "-1", "--format=%B"]);
    assert!(
        stdout_of(&log).contains("REWRITTEN"),
        "msg-filter did not run: {:?}",
        stdout_of(&log)
    );
    f.assert_shim_untouched();
}

/// `rebase --exec` is `do_exec()`'s `use_shell` child (sequencer.c:3870).
#[test]
fn rebase_exec_runs_under_the_absolute_shell() {
    let f = Fixture::new("exec");
    std::fs::write(f.repo.join("b.txt"), "b\n").unwrap();
    f.git(&["add", "b.txt"]);
    f.git(&["commit", "-q", "-m", "second"]);

    let out = f.git_shadowed(&["rebase", "--exec", "echo EXEC-REAL", "HEAD~1"]);
    assert!(
        out.status.success(),
        "rebase --exec failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let all = format!("{}{}", stdout_of(&out), String::from_utf8_lossy(&out.stderr));
    assert!(all.contains("EXEC-REAL"), "exec line did not run: {all:?}");
    assert!(!all.contains(FAKE_OUTPUT));
    f.assert_shim_untouched();
}

/// `git version --build-options` reports `shell-path:`. The value has to stay a
/// true statement about the binary: an absolute path that exists, and the one
/// that survives a shadowed `PATH` — never a bare `sh` the OS would resolve.
#[test]
fn reported_shell_path_is_the_one_actually_spawned() {
    let f = Fixture::new("version");
    let out = f.git_shadowed(&["version", "--build-options"]);
    assert!(out.status.success());

    let reported = stdout_of(&out)
        .lines()
        .find_map(|l| l.strip_prefix("shell-path: ").map(str::to_owned))
        .expect("build options report a shell-path");

    assert!(
        reported.starts_with('/'),
        "shell-path must be absolute, not a PATH-resolved name: {reported:?}"
    );
    assert!(
        Path::new(&reported).exists(),
        "reported shell {reported:?} does not exist"
    );
    // The shim is named `sh` and sits first on PATH; the reported path is not it.
    assert_ne!(Path::new(&reported), f.shadow.join("sh"));

    // And that reported shell is what a shell child really lands on: asking it to
    // identify itself must not produce the shim's output.
    let probe = f.git_shadowed(&["-c", "alias.zzid=!printf SHELL-OK", "zzid"]);
    assert_eq!(stdout_of(&probe).trim(), "SHELL-OK");
    f.assert_shim_untouched();
}
