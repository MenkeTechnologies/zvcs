//! `git daemon`'s accept loop: a real TCP client talks the `git://` protocol to
//! a daemon this test starts, and gets the answers `daemon.c`'s `handle()` /
//! `execute()` give — the service's ref advertisement for an exported
//! repository, an `ERR` packet for one that is not exported, and a second
//! connection served after the first has gone.
//!
//! The port is taken from the kernel (bind to 0, read it back, release it), so
//! the test needs only loopback and never collides with a real daemon on 9418.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

struct Daemon {
    root: PathBuf,
    child: Child,
    port: u16,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Daemon {
    /// Two repositories under one base path: `open` is exported (carries
    /// `git-daemon-export-ok`), `closed` is not.
    fn start(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-daemon-serve-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        for name in ["open", "closed"] {
            git(&root, &["init", "-q", name]);
            git(&root.join(name), &["commit", "-q", "--allow-empty", "-m", name]);
        }
        std::fs::write(root.join("open/.git/git-daemon-export-ok"), "").unwrap();

        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let child = Command::new(BIN)
            .arg("daemon")
            .arg(format!("--port={port}"))
            .arg("--listen=127.0.0.1")
            .arg(format!("--base-path={}", root.display()))
            .arg("--informative-errors")
            .current_dir(&root)
            .env("HOME", &root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let daemon = Daemon { root, child, port };
        let deadline = Instant::now() + Duration::from_secs(30);
        while TcpStream::connect(("127.0.0.1", port)).is_err() {
            assert!(Instant::now() < deadline, "daemon never started listening");
            std::thread::sleep(Duration::from_millis(50));
        }
        daemon
    }

    /// Send one `git-upload-pack <path>` request and read until the daemon closes.
    fn upload_pack(&self, path: &str) -> Vec<u8> {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        let body = format!("git-upload-pack {path}\0host=localhost\0");
        write!(stream, "{:04x}{body}", body.len() + 4).unwrap();
        // Flush packet: the client has nothing to ask, so the service ends.
        stream.write_all(b"0000").unwrap();
        let mut out = Vec::new();
        let _ = stream.read_to_end(&mut out);
        out
    }
}

/// An exported repository is answered with the v0 ref advertisement, and the
/// daemon keeps accepting afterwards.
#[test]
fn exported_repository_gets_the_ref_advertisement_on_every_connection() {
    let d = Daemon::start("export");
    for round in 0..2 {
        let reply = String::from_utf8_lossy(&d.upload_pack("/open")).into_owned();
        assert!(
            reply.contains(" HEAD\0") && reply.contains(" refs/heads/"),
            "round {round}: no ref advertisement in {reply:?}"
        );
    }
}

/// A repository without `git-daemon-export-ok` is refused with one `ERR` packet,
/// naming the reason because `--informative-errors` is on.
#[test]
fn unexported_repository_is_refused_with_an_err_packet() {
    let d = Daemon::start("refuse");
    let reply = String::from_utf8_lossy(&d.upload_pack("/closed")).into_owned();
    assert!(
        reply.starts_with("003dERR repository not exported: /closed")
            || reply.contains("ERR repository not exported: /closed"),
        "unexpected reply {reply:?}"
    );
}
