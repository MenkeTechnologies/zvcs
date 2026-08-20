//! `git help -m`'s viewer chain: `man.viewer`, `man.<tool>.cmd` and
//! `man.<tool>.path` (`builtin/help.c`'s `show_man_page()`, `exec_viewer()` and
//! `add_man_viewer_info()`).
//!
//! Every case here drives a *stand-in* viewer — `printf`, `/bin/echo`, a name
//! that does not exist — so no real pager, browser or GUI is ever started and
//! the test is safe headless. That is also what makes the assertions sharp: the
//! stand-in prints the page name it was handed, so its stdout is proof of both
//! *which* viewer ran and *what* argument it was built with.
//!
//! The behaviours pinned, all of them stock's:
//!
//!   * `man.viewer` is a **list**, tried in configuration order, and a viewer
//!     that cannot start is skipped rather than fatal — the chain ends at the
//!     built-in `man`, and only if that fails too is it
//!     `fatal: no man viewer handled the request`.
//!   * `man.<tool>.cmd` supplies a whole shell command for a viewer git does not
//!     know; the page name is appended to it.
//!   * `man.<tool>.path` overrides the program for the three viewers git *does*
//!     know (`man`, `woman`, `konqueror`).
//!   * Putting the value under the wrong one of those two keys is a warning
//!     naming the other key, and the value is dropped.
//!   * `$GIT_MAN_VIEWER` is consulted after the configured list and before the
//!     built-in `man`.
//!
//! Where a case is observable on stock without launching anything real (the
//! warnings, the `man.<tool>.cmd` echo), it is compared against
//! `/opt/homebrew/bin/git` byte for byte.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");
const STOCK: &str = "/opt/homebrew/bin/git";

fn stock_available() -> bool {
    Command::new(STOCK).arg("--version").output().is_ok_and(|o| o.status.success())
}

/// A repository with an isolated `$HOME`, so only the configuration each test
/// sets is read.
fn fixture(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-manviewer-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    std::fs::create_dir_all(root.join("home")).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(git(&repo, &["init", "-q", "-b", "main"]).status.success());
    repo
}

fn command(bin: &str, repo: &Path, args: &[&str]) -> Command {
    let home = repo.parent().unwrap().join("home");
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(repo)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", &home)
        .env("LC_ALL", "C")
        .env_remove("GIT_MAN_VIEWER")
        // No graphical session, so the konqueror viewer declines the way it does
        // on a headless machine unless a test asks otherwise.
        .env_remove("DISPLAY")
        .stdin(std::process::Stdio::null());
    cmd
}

fn git(repo: &Path, args: &[&str]) -> Output {
    command(BIN, repo, args).output().unwrap()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A viewer git does not know takes its whole command line from
/// `man.<tool>.cmd`, with the page name appended — `exec_man_cmd()` runs
/// `sh -c "<cmd> <page>"`.
#[test]
fn man_tool_cmd_runs_the_configured_command_line() {
    let repo = fixture("cmd");
    assert!(git(&repo, &["config", "man.viewer", "zvcsecho"]).status.success());
    assert!(git(&repo, &["config", "man.zvcsecho.cmd", "printf '%s\\n'"]).status.success());

    let out = git(&repo, &["help", "-m", "status"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "git-status\n", "the configured viewer did not receive the page");

    if stock_available() {
        let stock = command(STOCK, &repo, &["help", "-m", "status"]).output().unwrap();
        assert_eq!(out.stdout, stock.stdout, "diverges from stock");
    }
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `man.<tool>.path` overrides the *program* for a viewer git knows how to
/// drive. `exec_man_man()` keeps `argv[0]` as `man`, so a stand-in that echoes
/// its arguments prints only the page.
#[test]
fn man_tool_path_overrides_the_program() {
    let repo = fixture("path");
    assert!(git(&repo, &["config", "man.viewer", "man"]).status.success());
    assert!(git(&repo, &["config", "man.man.path", "/bin/echo"]).status.success());

    let out = git(&repo, &["help", "-m", "status"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "git-status\n");

    if stock_available() {
        let stock = command(STOCK, &repo, &["help", "-m", "status"]).output().unwrap();
        assert_eq!(out.stdout, stock.stdout, "diverges from stock");
    }
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// The two keys are not interchangeable: `path` is for the viewers git drives
/// itself, `cmd` for the ones it does not. The wrong pairing is a warning that
/// names the key that would have worked, and the value is *dropped* — which is
/// why the run below still falls through to the next viewer.
#[test]
fn mismatched_path_and_cmd_warn_the_way_stock_does() {
    let repo = fixture("mismatch");
    assert!(git(&repo, &["config", "man.zvcsecho.path", "/bin/echo"]).status.success());
    assert!(git(&repo, &["config", "man.man.cmd", "printf 'CMD:%s\\n'"]).status.success());
    // The `man` viewer's *path* is a stand-in, so the run never reaches a real
    // `man` — and its output tells the two keys apart: `CMD:` would mean the
    // rejected `man.man.cmd` had been honoured.
    assert!(git(&repo, &["config", "man.man.path", "/bin/echo"]).status.success());
    assert!(git(&repo, &["config", "man.viewer", "man"]).status.success());

    let out = git(&repo, &["help", "-m", "status"]);
    let text = stderr(&out);
    assert!(
        text.contains(
            "warning: 'zvcsecho.path': path for unsupported man viewer.\nPlease consider using 'man.<tool>.cmd' instead."
        ),
        "stderr: {text}"
    );
    assert!(
        text.contains(
            "warning: 'man.cmd': cmd for supported man viewer.\nPlease consider using 'man.<tool>.path' instead."
        ),
        "stderr: {text}"
    );
    // The dropped `man.man.cmd` means the `man` viewer ran through its `path`,
    // rather than the configured command line.
    assert_eq!(stdout(&out), "git-status\n", "a dropped `cmd` value was run anyway");

    if stock_available() {
        let stock = command(STOCK, &repo, &["help", "-m", "status"]).output().unwrap();
        let stock_text = stderr(&stock);
        for line in [
            "warning: 'zvcsecho.path': path for unsupported man viewer.",
            "warning: 'man.cmd': cmd for supported man viewer.",
        ] {
            assert!(stock_text.contains(line), "fixture assumption broken on stock: {stock_text}");
        }
    }
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `man.viewer` is a list, walked in configuration order: a viewer that cannot
/// be *started* is skipped with a warning and the next one is tried.
///
/// "Cannot be started" is narrower than "fails". A viewer named with no
/// `man.<tool>.cmd` is unknown and skipped, and a `man.<tool>.path` that cannot
/// be exec'd is skipped — but a `man.<tool>.cmd` naming a missing program is
/// *not*, because the shell starts fine and it is the shell that reports the
/// failure. Stock behaves the same way (exit 127, chain over), which is what the
/// last case pins.
#[test]
fn the_viewer_list_falls_through_in_configuration_order() {
    let repo = fixture("chain");
    // Three viewers: one with a `path` that cannot be exec'd, one with no `cmd`
    // at all, and one that works. Only the third may produce output.
    assert!(git(&repo, &["config", "--add", "man.viewer", "man"]).status.success());
    assert!(git(&repo, &["config", "--add", "man.viewer", "zvcsunknown"]).status.success());
    assert!(git(&repo, &["config", "--add", "man.viewer", "zvcsworks"]).status.success());
    assert!(git(&repo, &["config", "man.man.path", "/zvcs/no/such/man"]).status.success());
    assert!(git(&repo, &["config", "man.zvcsworks.cmd", "printf 'ran:%s\\n'"]).status.success());

    let out = git(&repo, &["help", "-m", "status"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "ran:git-status\n", "the wrong viewer in the chain answered");
    let text = stderr(&out);
    assert!(
        text.contains("warning: failed to exec '/zvcs/no/such/man': No such file or directory"),
        "an unstartable viewer should warn: {text}"
    );
    assert!(
        text.contains("warning: 'zvcsunknown': unknown man viewer."),
        "a viewer with no cmd should warn: {text}"
    );

    if stock_available() {
        let stock = command(STOCK, &repo, &["help", "-m", "status"]).output().unwrap();
        assert_eq!(out.stdout, stock.stdout, "diverges from stock");
        assert_eq!(out.stderr, stock.stderr, "warnings diverge from stock");
    }
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// A `man.<tool>.cmd` whose program does not exist is the *shell's* failure, not
/// a viewer that declined: `exec_man_cmd()` execs `$SHELL_PATH -c`, which starts,
/// so the chain ends there with the shell's 127 and its message. Asserted
/// against stock because it is the case most easily got wrong in the other
/// direction (silently trying the next viewer).
#[test]
fn a_cmd_naming_a_missing_program_ends_the_chain() {
    let repo = fixture("brokencmd");
    assert!(git(&repo, &["config", "--add", "man.viewer", "zvcsbroken"]).status.success());
    assert!(git(&repo, &["config", "--add", "man.viewer", "zvcsworks"]).status.success());
    assert!(git(&repo, &["config", "man.zvcsbroken.cmd", "/zvcs/no/such/program"]).status.success());
    assert!(git(&repo, &["config", "man.zvcsworks.cmd", "printf 'ran:%s\\n'"]).status.success());

    let out = git(&repo, &["help", "-m", "status"]);
    assert_eq!(out.status.code(), Some(127), "the shell's status must be the command's");
    assert!(stdout(&out).is_empty(), "the chain continued past a started shell");

    if stock_available() {
        let stock = command(STOCK, &repo, &["help", "-m", "status"]).output().unwrap();
        assert_eq!(out.status.code(), stock.status.code());
        assert_eq!(out.stdout, stock.stdout);
    }
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// When no viewer in the chain can be started at all, git `die`s — `fatal: ` on
/// stderr and exit 128, not this port's own error wrapper.
#[test]
fn no_viewer_at_all_dies_the_way_stock_dies() {
    let repo = fixture("noviewer");
    assert!(git(&repo, &["config", "man.viewer", "man"]).status.success());
    assert!(git(&repo, &["config", "man.man.path", "/zvcs/no/such/man"]).status.success());

    let out = git(&repo, &["help", "-m", "status"]);
    assert_eq!(out.status.code(), Some(128));
    assert!(
        stderr(&out).ends_with("fatal: no man viewer handled the request\n"),
        "stderr: {}",
        stderr(&out)
    );
    if stock_available() {
        let stock = command(STOCK, &repo, &["help", "-m", "status"]).output().unwrap();
        assert_eq!(out.status.code(), stock.status.code());
        assert_eq!(out.stderr, stock.stderr, "diverges from stock");
    }
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `$GIT_MAN_VIEWER` is the fallback after the configured list, before the
/// built-in `man` — `show_man_page()`'s last two candidates.
#[test]
fn git_man_viewer_env_is_tried_after_the_configured_list() {
    let repo = fixture("env");
    assert!(git(&repo, &["config", "man.viewer", "zvcsunknown"]).status.success());
    assert!(git(&repo, &["config", "man.zvcsenv.cmd", "printf 'env:%s\\n'"]).status.success());

    let out = command(BIN, &repo, &["help", "-m", "status"])
        .env("GIT_MAN_VIEWER", "zvcsenv")
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "env:git-status\n");

    if stock_available() {
        let stock = command(STOCK, &repo, &["help", "-m", "status"])
            .env("GIT_MAN_VIEWER", "zvcsenv")
            .output()
            .unwrap();
        assert_eq!(out.stdout, stock.stdout, "diverges from stock");
    }
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// The viewer chain serves the superset (`z*`) verbs too: their pages are
/// generated on demand and reached through `$MANPATH`, so a `man.<tool>.cmd`
/// viewer is handed the same page name a real `man` would resolve.
#[test]
fn superset_verbs_go_through_the_same_chain() {
    let repo = fixture("superset");
    assert!(git(&repo, &["config", "man.viewer", "zvcsecho"]).status.success());
    assert!(git(&repo, &["config", "man.zvcsecho.cmd", "printf '%s\\n'"]).status.success());

    let out = git(&repo, &["help", "-m", "zstatus"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "git-zstatus\n");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
