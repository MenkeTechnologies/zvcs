//! A failed `ssh` transport reports what git reports, byte for byte.
//!
//! git lets the `ssh` child's stderr through untouched and adds one fixed block:
//!
//! ```text
//! ssh: Could not resolve hostname nosuchhost.invalid: nodename nor servname provided, or not known
//! fatal: Could not read from remote repository.
//!
//! Please make sure you have the correct access rights
//! and the repository exists.
//! ```
//!
//! exiting 128. The vendored transport captures that line into the error chain
//! instead, so it has to be reprinted — including OpenSSH's CRLF terminator, which
//! the line-splitting capture drops and which is a real byte in git's output.
//!
//! An unresolvable `.invalid` host needs no network and no key, so this is
//! deterministic: DNS is guaranteed to fail for the reserved TLD.

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");
const URL: &str = "git@nosuchhost.invalid:someone/repo.git";

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes -o ConnectTimeout=5")
        .output()
        .expect("run binary")
}

/// The tail of git's message, which is fixed text.
const BLOCK: &str = "fatal: Could not read from remote repository.\n\n\
                     Please make sure you have the correct access rights\n\
                     and the repository exists.\n";

#[test]
fn ssh_failure_reports_gits_block_and_exits_128() {
    if which_ssh().is_none() {
        eprintln!("skipping: no ssh in PATH");
        return;
    }
    let root = std::env::temp_dir().join(format!("zvcs-sshfail-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let o = run(&root, &home, &["clone", URL, "dst"]);
    assert_eq!(o.status.code(), Some(128), "{}", String::from_utf8_lossy(&o.stderr));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.ends_with(BLOCK),
        "git's fixed block has to come last, verbatim:\n{err:?}"
    );
    // `Cloning into '<dst>'...` precedes it on stderr, then the ssh child's own
    // line, terminated the way OpenSSH terminates it — the byte a line-splitting
    // capture loses.
    assert!(
        err.contains("\nssh: Could not resolve hostname nosuchhost.invalid"),
        "{err:?}"
    );
    assert!(
        err.contains("\r\nfatal: Could not read from remote repository."),
        "the ssh line keeps its CRLF: {err:?}"
    );
    // Nothing of zvcs's own error wrapper survives.
    assert!(!err.contains("zvcs:"), "{err:?}");

    // The same failure through the other remote-touching verbs.
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["remote", "add", "origin", URL][..],
    ] {
        assert!(run(&repo, &home, args).status.success(), "setup {args:?}");
    }
    for args in [
        &["ls-remote", "origin"][..],
        &["fetch", "origin"][..],
        &["remote", "show", "origin"][..],
    ] {
        let o = run(&repo, &home, args);
        let err = String::from_utf8_lossy(&o.stderr);
        assert_eq!(o.status.code(), Some(128), "{args:?}: {err}");
        assert!(err.ends_with(BLOCK), "{args:?}: {err:?}");
        assert!(!err.contains("zvcs:"), "{args:?}: {err:?}");
    }

    let _ = std::fs::remove_dir_all(&root);
}

fn which_ssh() -> Option<std::path::PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join("ssh"))
            .find(|p| p.is_file())
    })
}
