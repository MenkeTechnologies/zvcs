//! `git remote-ext` — the parts of the helper protocol and of `run_child()` that
//! stock git gets from `builtin/remote-ext.c`, `transport-helper.c` and
//! `run-command.c`, each pinned against the transcript measured from git 2.55.0
//! on the same fixture.
//!
//! Every expectation below was produced by running `/usr/local/bin/git` (2.55.0)
//! on the shell scripts these tests write; the constants are that transcript, not
//! a reading of the source. The citations say where the source agrees.
//!
//! No network, no sockets, no repository: `remote-ext` reads its command loop on
//! stdin and runs a local `/bin/sh` script as the transport. Every child is
//! reaped or killed before the test returns.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A per-test scratch directory, removed on the way out.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "zvcs-remoteext-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Scratch(p)
    }

    /// Write `body` as an executable `/bin/sh` script and return its path.
    fn script(&self, name: &str, body: &str) -> PathBuf {
        let p = self.0.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}")).unwrap();
        perm(&p, 0o755);
        p
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn perm(p: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// One `git remote-ext origin <url>` run, fed `stdin` and driven to completion.
/// Returns `(exit code, stdout, stderr)`; the exit code is the raw wait status
/// code, which is what a shell reports in `$?`.
fn run_ext(url: &Path, stdin: &[u8]) -> (i32, Vec<u8>, String) {
    run_ext_raw(url.to_string_lossy().as_ref(), stdin)
}

fn run_ext_raw(url: &str, stdin: &[u8]) -> (i32, Vec<u8>, String) {
    let mut child = Command::new(BIN)
        .args(["remote-ext", "origin", url])
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

/// `builtin/remote-ext.c`'s `command_loop()` compares the line with `strcmp`
/// after `fgets` has stored it in a `char[]`, so the command ends at the first
/// NUL. Stock git 2.55.0 on `printf 'capabilities\0junk\n'`:
///
/// ```text
/// rc=0  stdout='*connect\n\n'  stderr=''
/// ```
#[test]
fn embedded_nul_ends_the_command_line() {
    let s = Scratch::new("nul");
    let cmd = s.script("never-run.sh", "exit 3\n");

    let (rc, out, err) = run_ext(&cmd, b"capabilities\0junk\n");
    assert_eq!(rc, 0, "stderr: {err}");
    assert_eq!(out, b"*connect\n\n", "stdout: {:?}", String::from_utf8_lossy(&out));
    assert_eq!(err, "");
}

/// The same truncation on the `connect` line: `connect git-upload-pack\0x` is
/// `connect git-upload-pack`, so the service name handed to the child (and to
/// `GIT_EXT_SERVICE`) carries no NUL. Measured from stock git: the child runs and
/// reports the clean service name.
#[test]
fn embedded_nul_does_not_reach_the_service_name() {
    let s = Scratch::new("nulsvc");
    let cmd = s.script("svc.sh", "printf 'SVC=[%s]\\n' \"$GIT_EXT_SERVICE\"\n");

    let (rc, out, err) = run_ext(&cmd, b"connect git-upload-pack\0x\n");
    assert_eq!(rc, 0, "stderr: {err}");
    assert_eq!(
        String::from_utf8_lossy(&out),
        "\nSVC=[git-upload-pack]\n",
        "stderr: {err}"
    );
}

/// git's `isspace()` is not the C library's: `sane-ctype.h:40` redefines it as
/// `sane_istest(x, GIT_SPACE)` and `ctype.c`'s table gives `GIT_SPACE` only to
/// 0x09, 0x0a, 0x0d and 0x20 — a vertical tab and a form feed are `GIT_CNTRL`
/// alone. So `command_loop()`'s strip loop leaves them in place and the line is
/// no longer `capabilities`. Stock git 2.55.0 on `printf 'capabilities\v\n'`:
///
/// ```text
/// rc=1  stdout=''  stderr='Bad command'
/// ```
///
/// (`fprintf(stderr, "Bad command")` carries no newline, and `command_loop()`
/// returns 1.)
#[test]
fn vertical_tab_and_form_feed_are_not_whitespace() {
    let s = Scratch::new("vtab");
    let cmd = s.script("never-run.sh", "exit 3\n");

    for trailer in [b"\x0b".as_slice(), b"\x0c".as_slice()] {
        let mut line = b"capabilities".to_vec();
        line.extend_from_slice(trailer);
        line.push(b'\n');

        let (rc, out, err) = run_ext(&cmd, &line);
        assert_eq!(rc, 1, "trailer {trailer:?}");
        assert!(out.is_empty(), "trailer {trailer:?}: stdout {out:?}");
        assert_eq!(err, "Bad command", "trailer {trailer:?}");
    }
}

/// The control for the test above: the four bytes git *does* call whitespace are
/// stripped, so the command is still `capabilities`.
#[test]
fn tab_cr_and_space_are_stripped_from_the_command_line() {
    let s = Scratch::new("strip");
    let cmd = s.script("never-run.sh", "exit 3\n");

    for trailer in [
        b"\t".as_slice(),
        b"\r".as_slice(),
        b"   ".as_slice(),
        b" \t\r ".as_slice(),
    ] {
        let mut line = b"capabilities".to_vec();
        line.extend_from_slice(trailer);
        line.push(b'\n');

        let (rc, out, err) = run_ext(&cmd, &line);
        assert_eq!(rc, 0, "trailer {trailer:?}: stderr {err}");
        assert_eq!(out, b"*connect\n\n", "trailer {trailer:?}");
    }
}

/// A command name that contains a directory separator is handed straight to
/// `execve`, and the failure comes back through `child_err_spew()`
/// (run-command.c:404) — which runs `error_errno("cannot exec '%s'", …)` with the
/// *die* message routine installed (run-command.c:381-384), so the prefix is
/// `fatal:` and the name is quoted. `run_child()` then adds its own
/// `die("Can't run specified command")`. Stock git 2.55.0, on a `0644` script:
///
/// ```text
/// rc=128  stderr="fatal: cannot exec '<path>': Permission denied\n\
///                 fatal: Can't run specified command\n"
/// ```
#[test]
fn exec_failure_is_reported_as_cannot_exec() {
    let s = Scratch::new("noexec");
    let cmd = s.script("noperm.sh", "echo hi\n");
    perm(&cmd, 0o644);

    let (rc, out, err) = run_ext(&cmd, b"connect git-upload-pack\n");
    assert_eq!(rc, 128, "stderr: {err}");
    // The `\n` acknowledgement is written before the child is spawned.
    assert_eq!(out, b"\n");
    assert_eq!(
        err,
        format!(
            "fatal: cannot exec '{}': Permission denied\nfatal: Can't run specified command\n",
            cmd.display()
        )
    );
}

/// The same for a name that does not exist at all — still `cannot exec`, because
/// `prepare_cmd()` leaves a name with a separator alone (run-command.c:435).
#[test]
fn missing_command_with_a_path_is_also_cannot_exec() {
    let s = Scratch::new("missing");
    let cmd = s.0.join("no-such-helper");

    let (rc, _out, err) = run_ext(&cmd, b"connect git-upload-pack\n");
    assert_eq!(rc, 128, "stderr: {err}");
    assert_eq!(
        err,
        format!(
            "fatal: cannot exec '{}': No such file or directory\nfatal: Can't run specified command\n",
            cmd.display()
        )
    );
}

/// A *bare* command name takes the other branch: `prepare_cmd()` resolves it
/// through `locate_in_PATH()` before the fork, and `is_executable()`
/// (run-command.c:131-137) accepts only a regular file with the owner execute
/// bit. A `0644` candidate is therefore skipped, the lookup finds nothing, and
/// `start_command()` reports `ENOENT` regardless of the real reason
/// (run-command.c:757-762) — `error:`, not `fatal:`, and no quotes.
///
/// Stock git 2.55.0 with `$PATH` holding a `0644` `hs_nonexec_cmd`:
///
/// ```text
/// rc=128  stderr="error: cannot run hs_nonexec_cmd: No such file or directory\n\
///                 fatal: Can't run specified command\n"
/// ```
///
/// Reporting `Permission denied` here — the naive `execvp` errno — is the shape
/// this test exists to reject.
#[test]
fn path_lookup_skips_non_executables_and_reports_enoent() {
    let s = Scratch::new("pathmiss");
    let bin = s.0.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let cmd = bin.join("zvcs_nonexec_cmd");
    std::fs::write(&cmd, "#!/bin/sh\necho hi\n").unwrap();
    perm(&cmd, 0o644);

    let mut child = Command::new(BIN)
        .args(["remote-ext", "origin", "zvcs_nonexec_cmd"])
        .env("PATH", &bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"connect git-upload-pack\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();

    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "error: cannot run zvcs_nonexec_cmd: No such file or directory\n\
         fatal: Can't run specified command\n"
    );
}

/// The control: the same lookup *succeeds* once the candidate is executable, and
/// the resolved program runs.
#[test]
fn path_lookup_finds_an_executable_candidate() {
    let s = Scratch::new("pathhit");
    let bin = s.0.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let cmd = bin.join("zvcs_exec_cmd");
    std::fs::write(&cmd, "#!/bin/sh\necho FOUND\n").unwrap();
    perm(&cmd, 0o755);

    let mut child = Command::new(BIN)
        .args(["remote-ext", "origin", "zvcs_exec_cmd"])
        .env("PATH", &bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"connect git-upload-pack\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();

    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "\nFOUND\n");
}

/// `wait_or_whine()` (run-command.c:574-583) announces a signal death unless the
/// signal is `SIGINT`, `SIGQUIT` **or `SIGPIPE`**; the exit code is
/// `128 + signal` either way. Measured from stock git 2.55.0 for a child that
/// kills itself:
///
/// ```text
/// TERM (15): rc=143  stderr="error: <path> died of signal 15\n"
/// PIPE (13): rc=141  stderr=""
/// INT   (2): rc=130  stderr=""
/// QUIT  (3): rc=131  stderr=""
/// ```
#[test]
fn signal_death_is_announced_except_for_int_quit_and_pipe() {
    let s = Scratch::new("signal");

    for (name, number, announced) in [
        ("TERM", 15, true),
        ("PIPE", 13, false),
        ("INT", 2, false),
        ("QUIT", 3, false),
    ] {
        let cmd = s.script(&format!("kill-{name}.sh"), &format!("kill -{name} $$\n"));
        let (rc, _out, err) = run_ext(&cmd, b"connect git-upload-pack\n");

        assert_eq!(rc, 128 + number, "signal {name}: stderr {err}");
        if announced {
            assert_eq!(
                err,
                format!("error: {} died of signal {number}\n", cmd.display()),
                "signal {name}"
            );
        } else {
            assert_eq!(err, "", "signal {name} must not be announced");
        }
    }
}

/// `bidirectional_transfer_loop(child.out, child.in)` (remote-ext.c:154) gives
/// the program-to-git half `dest = 1`, and `udt_close_if_finished()`
/// (transport-helper.c:1421-1430) `close()`s that destination as soon as the
/// source hits EOF. So when the transport child exits, stock `remote-ext` closes
/// **its own stdout** even though it is still blocked draining stdin — a caller
/// that holds the helper's stdin open sees end-of-stream immediately.
///
/// Measured against stock git 2.55.0 with the driver below: stdout reaches EOF
/// carrying `"\nPAYLOAD\n"` while the helper is still running. Not closing it is
/// a deadlock for any caller that waits for EOF before closing the helper's
/// stdin, which is why this test takes a timeout rather than an assertion.
#[test]
fn stdout_is_closed_when_the_child_stdout_ends() {
    let s = Scratch::new("closeout");
    let cmd = s.script("quick.sh", "printf 'PAYLOAD\\n'\nexit 0\n");

    let mut child = Command::new(BIN)
        .args(["remote-ext", "origin", cmd.to_string_lossy().as_ref()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    // Deliberately kept open for the whole test: this is the caller that stock
    // git releases by closing its stdout.
    let mut to_helper = child.stdin.take().unwrap();
    to_helper.write_all(b"connect git-upload-pack\n").unwrap();
    to_helper.flush().unwrap();

    let mut from_helper = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let r = from_helper.read_to_end(&mut buf);
        let _ = tx.send(r.map(|_| buf));
    });

    let got = rx.recv_timeout(Duration::from_secs(20));
    let outcome = got.map(|r| r.unwrap());
    reap(&mut child);

    match outcome {
        Ok(bytes) => assert_eq!(
            String::from_utf8_lossy(&bytes),
            "\nPAYLOAD\n",
            "stdout reached EOF but with the wrong bytes"
        ),
        Err(_) => panic!(
            "stdout never reached EOF while the helper's stdin was still open; \
             stock git closes fd 1 from udt_close_if_finished()"
        ),
    }
}

/// Kill and reap, so no test leaves a `remote-ext` behind.
fn reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}
