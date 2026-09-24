//! Only `store` carries `FLAG_SPAWN` (builtin/credential-cache.c:176-180). A
//! `get` or `erase` with no daemon listening fails to connect with `ENOENT` or
//! `ECONNREFUSED`, which `connection_fatally_broken()` forgives, and returns 0
//! without starting anything. The port spawned a daemon for all three, so an
//! `erase` left a daemon behind — and two concurrent ones raced to bind the same
//! socket, one dying with `Address already in use`.
//!
//! Every expectation was measured against stock git 2.55.0.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

const CREDENTIAL: &str = "protocol=https\nhost=example.com\nusername=u\n\n";

fn run(dir: &Path, socket: &Path, action: &str) -> Output {
    let mut child = Command::new(BIN)
        .arg("credential-cache")
        .arg("--socket")
        .arg(socket)
        .arg(action)
        .current_dir(dir)
        .env("HOME", dir)
        .env("ZVCS_HOME", dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(CREDENTIAL.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

/// A short private directory: the socket path must fit in `sun_path`.
fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zcc-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

fn assert_quiet_without_daemon(dir: &Path, socket: &Path, action: &str) {
    let out = run(dir, socket, action);
    assert_eq!(out.status.code(), Some(0), "{action}: {}", String::from_utf8_lossy(&out.stderr));
    assert!(out.stdout.is_empty(), "{action}: nothing to answer");
    assert!(out.stderr.is_empty(), "{action}: nothing to report");
    assert!(!socket.exists(), "{action}: no daemon may be started");
}

#[test]
fn get_and_erase_never_start_a_daemon() {
    let dir = fixture("none");
    let socket = dir.join("s");
    assert_quiet_without_daemon(&dir, &socket, "get");
    assert_quiet_without_daemon(&dir, &socket, "erase");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_stale_socket_is_refused_quietly() {
    // A daemon killed without cleanup leaves its socket file: connecting to it is
    // `ECONNREFUSED`, forgiven the same way as a missing one.
    let dir = fixture("stale");
    let socket = dir.join("s");
    drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
    assert!(socket.exists());
    for action in ["get", "erase"] {
        let out = run(&dir, &socket, action);
        assert_eq!(out.status.code(), Some(0), "{action}");
        assert!(out.stdout.is_empty() && out.stderr.is_empty(), "{action}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
