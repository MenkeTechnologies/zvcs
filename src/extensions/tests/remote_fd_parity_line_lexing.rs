//! `git remote-fd` — the helper command loop's lexing, and the descriptor
//! round-trip through `bidirectional_transfer_loop()`.
//!
//! Every expectation was measured from stock git 2.55.0 driven the same way; the
//! citations below say where `builtin/remote-fd.c`, `transport-helper.c` and
//! `ctype.c` agree.
//!
//! No network and no sockets: the "remote" is a pair of ordinary file
//! descriptors, wired by a `/bin/sh` wrapper that redirects a file onto each and
//! then `exec`s the helper. Every child is reaped before the test returns.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "zvcs-remotefd-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Scratch(p)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `git remote-fd origin <url>` with both descriptors pointed at `/dev/null`, so
/// only the command loop runs. Returns `(code, stdout, stderr)`.
fn run_fd(url: &str, stdin: &[u8]) -> (i32, Vec<u8>, String) {
    let mut child = Command::new(BIN)
        .args(["remote-fd", "origin", url])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        out.stdout,
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `command_loop()` strips trailing whitespace with git's `isspace`, which
/// `sane-ctype.h:40` redefines as `sane_istest(x, GIT_SPACE)`. `ctype.c`'s table
/// row for bytes 0..15 is `X, X, X, X, X, X, X, X, X, Z, Z, X, X, Z, X, X`, so
/// only 0x09, 0x0a and 0x0d (plus 0x20) carry `GIT_SPACE`: a vertical tab (0x0b)
/// and a form feed (0x0c) are control characters, not whitespace, and stay on the
/// line. Stock git 2.55.0 on `printf 'capabilities\v\n'`:
///
/// ```text
/// rc=128  stdout=''  stderr='fatal: Bad command: capabilities\v\n'
/// ```
///
/// (`die("Bad command: %s", buffer)` echoes the line as it stands.)
#[test]
fn vertical_tab_and_form_feed_stay_on_the_command_line() {
    for trailer in [0x0bu8, 0x0c] {
        let mut line = b"capabilities".to_vec();
        line.push(trailer);
        line.push(b'\n');

        let (rc, out, err) = run_fd("1", &line);
        assert_eq!(rc, 128, "trailer {trailer:#04x}: stderr {err:?}");
        assert!(out.is_empty(), "trailer {trailer:#04x}: stdout {out:?}");
        assert_eq!(
            err.as_bytes(),
            {
                let mut want = b"fatal: Bad command: capabilities".to_vec();
                want.push(trailer);
                want.push(b'\n');
                want
            },
            "trailer {trailer:#04x}"
        );
    }
}

/// The control: the bytes git *does* treat as whitespace are stripped, so the
/// same line is recognised.
#[test]
fn git_whitespace_is_stripped_from_the_command_line() {
    for trailer in [b"\t".as_slice(), b"\r".as_slice(), b"  \t ".as_slice()] {
        let mut line = b"capabilities".to_vec();
        line.extend_from_slice(trailer);
        line.push(b'\n');

        let (rc, out, err) = run_fd("1", &line);
        assert_eq!(rc, 0, "trailer {trailer:?}: stderr {err}");
        assert_eq!(out, b"*connect\n\n", "trailer {trailer:?}");
    }
}

/// `fd::<infd>,<outfd>` with two ordinary pipes: after the `\n` acknowledgement,
/// `bidirectional_transfer_loop()` (transport-helper.c:1641) copies `<infd>` to
/// stdout and stdin to `<outfd>`, and closes each destination when its source
/// ends (`udt_close_if_finished`, transport-helper.c:1421-1430).
///
/// Driving it through `sh -c 'exec 7<in 8>out; exec git remote-fd …'` is the
/// shape git's own `t5802` uses. The payload is written only after the `\n`
/// acknowledgement has been read back, which is what every real caller does —
/// see [`payload_sharing_the_connect_write_survives`] for why that matters.
///
/// Stock git 2.55.0 on this fixture, measured: `rc=0`, stdout `"\nFROM-REMOTE\n"`,
/// and `TO-REMOTE\n` in the output file.
#[test]
fn descriptor_pair_round_trips_in_both_directions() {
    let s = Scratch::new("roundtrip");
    let infile = s.0.join("from-remote");
    let outfile = s.0.join("to-remote");
    std::fs::write(&infile, "FROM-REMOTE\n").unwrap();

    let mut child = spawn_fd(&infile, &outfile, "7,8/desc");

    let mut to_helper = child.stdin.take().unwrap();
    to_helper.write_all(b"connect git-upload-pack\n").unwrap();
    to_helper.flush().unwrap();

    let mut from_helper = child.stdout.take().unwrap();
    let mut ack = [0u8; 1];
    from_helper.read_exact(&mut ack).unwrap();
    assert_eq!(&ack, b"\n", "connect was not acknowledged");

    to_helper.write_all(b"TO-REMOTE\n").unwrap();
    drop(to_helper);

    let mut rest = Vec::new();
    from_helper.read_to_end(&mut rest).unwrap();
    let status = child.wait().unwrap();

    assert_eq!(status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&rest),
        "FROM-REMOTE\n",
        "the <infd> side did not reach stdout"
    );
    assert_eq!(
        read(&outfile),
        "TO-REMOTE\n",
        "stdin past the connect line did not reach <outfd>"
    );
}

/// A **deliberate divergence**, pinned so it cannot regress silently in either
/// direction.
///
/// Stock `remote-fd` reads its command loop with `fgets`, so bytes that arrive in
/// the same `read(2)` as the `connect` line are pulled into the `FILE` buffer and
/// never seen again by the transfer loop, which reads the raw descriptor.
/// Measured from git 2.55.0 with the connect line and the payload in one write:
///
/// ```text
/// rc=0  stdout='\nFROM-REMOTE\n'  <outfd> file = ''      <- payload lost
/// ```
///
/// and with the payload written only after the acknowledgement was read back:
///
/// ```text
/// rc=0  stdout='\nFROM-REMOTE\n'  <outfd> file = 'TO-REMOTE\n'
/// ```
///
/// This port reads descriptor 0 unbuffered, so the payload survives either way.
/// Emulating the loss would mean emulating one libc's buffer sizing, and no real
/// caller queues protocol data behind the line it is still waiting to have
/// acknowledged.
#[test]
fn payload_sharing_the_connect_write_survives() {
    let s = Scratch::new("nobuffer");
    let infile = s.0.join("from-remote");
    let outfile = s.0.join("to-remote");
    std::fs::write(&infile, "").unwrap();

    let payload = "0123456789abcdef".repeat(256);
    let mut stdin = b"connect git-upload-pack\n".to_vec();
    stdin.extend_from_slice(payload.as_bytes());

    let mut child = spawn_fd(&infile, &outfile, "7,8");
    {
        let mut to_helper = child.stdin.take().unwrap();
        to_helper.write_all(&stdin).unwrap();
    }
    let out = child.wait_with_output().unwrap();

    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(read(&outfile), payload);
}

/// `sh -c 'exec 7<in 8>out; exec git remote-fd origin <url>'`, with all three
/// standard streams piped.
fn spawn_fd(infile: &Path, outfile: &Path, url: &str) -> Child {
    Command::new("/bin/sh")
        .arg("-c")
        .arg(r#"exec 7<"$1" 8>"$2"; exec "$3" remote-fd origin "$4""#)
        .arg("sh")
        .arg(infile)
        .arg(outfile)
        .arg(BIN)
        .arg(url)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap()
}
