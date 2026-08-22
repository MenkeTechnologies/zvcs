//! An ssh transport that never got as far as talking prints git's block, and
//! nothing else.
//!
//! `git_connect()` (connect.c) hands the `ssh` child the caller's stderr, so
//! whatever the child said is already on the terminal when the protocol read
//! fails, and `die_initial_contact()` (connect.c:81-93) adds one fixed block:
//!
//! ```text
//! fatal: Could not read from remote repository.
//!
//! Please make sure you have the correct access rights
//! and the repository exists.
//! ```
//!
//! No line about the read itself. The port has to work for the same bytes
//! because the vendored transport intercepts the child's stderr — see
//! `crate::transport_err` — and the interception is where the defect was: an
//! `io::Error` that stood for the *stream ending* was reprinted as though it were
//! the child's words, so
//!
//! ```text
//! ERROR: Repository not found.
//! failed to fill whole buffer
//! fatal: Could not read from remote repository.
//! ```
//!
//! reached the terminal for a missing repository over ssh. `failed to fill whole
//! buffer` is `std`'s text for `read_exact` at EOF. git has no such message, and
//! a Rust diagnostic in a git transcript is a divergence a script grepping stderr
//! will see.
//!
//! Every expectation below was captured from stock git 2.55.0 driven with the
//! same `GIT_SSH_COMMAND`, which is what makes this headless: no network, no ssh
//! client, no host — a shell script stands in for `ssh` and writes exactly what a
//! real one writes. The four scripts cover the four shapes the supervisor
//! distinguishes:
//!
//! | child stderr | `line_to_err()` | what git prints |
//! |---|---|---|
//! | `ERROR: Repository not found.` | unrecognised, echoed live | the line, then the block |
//! | nothing at all | nothing | the block alone |
//! | `Connection closed by <host> port 22` | recognised, swallowed | the line, then the block |
//! | `ssh: Could not resolve hostname …` | recognised, swallowed | the line, then the block |

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// git's fixed block, byte for byte.
const BLOCK: &str = "fatal: Could not read from remote repository.\n\n\
                     Please make sure you have the correct access rights\n\
                     and the repository exists.\n";

fn scratch(tag: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let root = std::env::temp_dir().join(format!("zvcs-sshblock-{tag}-{}-{unique}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home")).expect("mkdir fixture");
    std::fs::create_dir_all(root.join("repo")).expect("mkdir repo");
    root.canonicalize().expect("canonicalize fixture")
}

fn run(dir: &Path, home: &Path, ssh: Option<&Path>, args: &[&str]) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "true")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE");
    if let Some(path) = ssh {
        cmd.env("GIT_SSH_COMMAND", path);
    }
    cmd.output().expect("run binary")
}

/// A stand-in for `ssh`: writes `stderr_text` verbatim and exits non-zero,
/// without ever speaking the protocol on stdout. That is exactly what a real
/// `ssh` does when the server refuses the request.
fn fake_ssh(root: &Path, name: &str, stderr_text: &str) -> PathBuf {
    let path = root.join(name);
    let body = if stderr_text.is_empty() {
        "#!/bin/sh\nexit 255\n".to_string()
    } else {
        // `printf` rather than `echo` so the exact bytes — CRLF included — are
        // under the test's control rather than the shell's.
        format!("#!/bin/sh\nprintf '{stderr_text}' >&2\nexit 255\n")
    };
    std::fs::write(&path, body).expect("write fake ssh");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    path
}

fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = scratch(tag);
    let (home, repo) = (root.join("home"), root.join("repo"));
    let out = run(&repo, &home, None, &["init", "-q", "-b", "main"]);
    assert!(out.status.success(), "init: {}", String::from_utf8_lossy(&out.stderr));
    (root, repo, home)
}

/// The reported shape: a repository the server does not have. `ssh` says so on
/// its own stderr, git echoes nothing of its own, and the block follows.
#[test]
fn an_unrecognised_ssh_diagnostic_is_followed_by_the_block_and_nothing_else() {
    let (root, repo, home) = fixture("notfound");
    let ssh = fake_ssh(&root, "ssh-notfound.sh", "ERROR: Repository not found.\\n");

    let out = run(&repo, &home, Some(&ssh), &["fetch", "ssh://example.invalid/x"]);

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(128), "stderr: {stderr:?}");
    assert_eq!(stderr, format!("ERROR: Repository not found.\n{BLOCK}"));
    assert!(
        !stderr.contains("failed to fill whole buffer"),
        "a Rust io::Error must never reach the terminal: {stderr:?}"
    );
}

/// An `ssh` that says nothing at all — a wrapper that fails silently, a
/// `ProxyCommand` that exits. git prints the block on its own.
#[test]
fn a_silent_ssh_prints_the_block_alone() {
    let (root, repo, home) = fixture("silent");
    let ssh = fake_ssh(&root, "ssh-silent.sh", "");

    let out = run(&repo, &home, Some(&ssh), &["fetch", "ssh://example.invalid/x"]);

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(128), "stderr: {stderr:?}");
    assert_eq!(stderr, BLOCK);
}

/// The other half of the rule: a line `line_to_err()` *does* recognise is
/// swallowed by the supervisor rather than echoed, so the port has to reprint it
/// — dropping the reprint entirely would lose a line git shows.
///
/// Both cases here are recognised (`Connection closed by ` and `resolve
/// hostname`), and both carry OpenSSH's CRLF, which is the byte sequence a real
/// client writes and which the round trip through the error must preserve.
#[test]
fn a_recognised_ssh_diagnostic_is_reprinted_before_the_block() {
    let (root, repo, home) = fixture("recognised");

    for (name, text, expected) in [
        (
            "ssh-closed.sh",
            "Connection closed by 140.82.121.4 port 22\\r\\n",
            "Connection closed by 140.82.121.4 port 22\r\n",
        ),
        (
            "ssh-resolve.sh",
            "ssh: Could not resolve hostname nosuchhost: nodename nor servname provided, or not known\\r\\n",
            "ssh: Could not resolve hostname nosuchhost: nodename nor servname provided, or not known\r\n",
        ),
    ] {
        let ssh = fake_ssh(&root, name, text);
        let out = run(&repo, &home, Some(&ssh), &["fetch", "ssh://example.invalid/x"]);

        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(128), "{name} stderr: {stderr:?}");
        assert_eq!(stderr, format!("{expected}{BLOCK}"), "{name}");
    }
}

/// `ls-remote` and `clone` reach the same reporter, so the same rule has to hold
/// for them. Only the absence of the Rust text is asserted for `clone`, whose
/// stdout carries a `Cloning into …` line of its own.
#[test]
fn the_other_ssh_callers_report_the_same_way() {
    let (root, repo, home) = fixture("callers");
    let ssh = fake_ssh(&root, "ssh-notfound.sh", "ERROR: Repository not found.\\n");

    let out = run(&repo, &home, Some(&ssh), &["ls-remote", "ssh://example.invalid/x"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(128), "ls-remote stderr: {stderr:?}");
    assert_eq!(stderr, format!("ERROR: Repository not found.\n{BLOCK}"), "ls-remote");

    let dest = root.join("clone-dest");
    let out = run(
        &repo,
        &home,
        Some(&ssh),
        &["clone", "ssh://example.invalid/x", dest.to_str().expect("utf-8 path")],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(128), "clone stderr: {stderr:?}");
    assert!(
        !stderr.contains("failed to fill whole buffer"),
        "clone leaked a Rust io::Error: {stderr:?}"
    );
    assert!(stderr.ends_with(BLOCK), "clone: {stderr:?}");
}
