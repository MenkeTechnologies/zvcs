//! The client's `unix_stream_connect()` and the daemon's `unix_stream_listen()`
//! share `unix_sockaddr_init()` (unix-socket.c:35-78): a path longer than
//! `sun_path` (104 bytes on macOS, 108 on Linux) is reached by `chdir`ing to its
//! directory and using the basename. The client connected by the full path, so a
//! `store` under a deep directory spawned a daemon it then could not reach and
//! died with a hardcoded `Connection refused`.
//!
//! `connection_fatally_broken()` (builtin/credential-cache.c:36-39) forgives only
//! `ENOENT` and `ECONNREFUSED`; any other connect error is
//! `die_errno("unable to connect to cache daemon")`. The client forgave them all.
//!
//! Every expectation was measured against stock git 2.55.0.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(home: &Path, socket: &Path, action: &str, input: &str) -> Output {
    let mut child = Command::new(BIN)
        .arg("credential-cache")
        .arg("--socket")
        .arg(socket)
        .arg(action)
        .current_dir(home)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

/// A private directory under a short root, so the length of the socket path is
/// decided by the test and not by `$TMPDIR`.
fn fixture(tag: &str) -> PathBuf {
    let dir = PathBuf::from(format!("/tmp/zccs-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

#[test]
fn a_socket_path_longer_than_sun_path_round_trips() {
    let home = fixture("long");
    let deep = home.join("d".repeat(60)).join("e".repeat(40));
    std::fs::create_dir_all(&deep).unwrap();
    // The daemon refuses a socket directory others can read.
    std::fs::set_permissions(&deep, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    let socket = deep.join("socket");
    assert!(socket.as_os_str().len() > 108, "{}", socket.display());

    let store = run(&home, &socket, "store", "protocol=https\nhost=example.com\nusername=u\npassword=p\n\n");
    assert_eq!(store.status.code(), Some(0), "{}", String::from_utf8_lossy(&store.stderr));
    let get = run(&home, &socket, "get", "protocol=https\nhost=example.com\n\n");
    let exit = run(&home, &socket, "exit", "");
    assert_eq!(get.status.code(), Some(0), "{}", String::from_utf8_lossy(&get.stderr));
    assert_eq!(
        String::from_utf8_lossy(&get.stdout),
        "capability[]=authtype\nusername=u\npassword=p\n"
    );
    assert_eq!(exit.status.code(), Some(0));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_connect_error_other_than_no_daemon_is_fatal() {
    let home = fixture("broken");
    let file = home.join("f");
    std::fs::write(&file, "").unwrap();
    for (socket, reason) in [
        (file.join("x"), "Not a directory"),
        // Even the basename does not fit, so `unix_sockaddr_init()` gives up.
        (home.join("q".repeat(120)), "File name too long"),
    ] {
        let out = run(&home, &socket, "get", "protocol=https\nhost=example.com\n\n");
        assert_eq!(out.status.code(), Some(128), "{}", socket.display());
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            format!("fatal: unable to connect to cache daemon: {reason}\n")
        );
    }
    let _ = std::fs::remove_dir_all(&home);
}
