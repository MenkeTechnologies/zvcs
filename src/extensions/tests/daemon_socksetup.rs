//! `git daemon`'s startup, up to and including the sockets it fails to open.
//!
//! `daemon` has a hand-rolled argument loop rather than `parse_options`, so its
//! refusals have shapes nothing else in git has: no `error: unknown option`
//! line, no abbreviation matching, and — because it has no `-h` case — `-h` and
//! a misspelled option produce the same usage block on stderr with exit 129.
//! Past parsing it runs `serve()`, whose first step is `socksetup()`; a port
//! that stops before that reports the wrong thing for the most common way
//! `git daemon` fails, which is a port that is already taken.
//!
//! Everything here is deliberately reachable **without binding anything**. The
//! bind-failure path is provoked with a port number that is not a port at all,
//! so name resolution fails before a socket is created: no listener is opened,
//! no port is occupied, and the test is safe to run in parallel on a shared or
//! sandboxed CI machine with no network. Expectations are what stock git 2.55.0
//! printed for the same argv (its resolver text differs per platform and is not
//! asserted — the exit code, the stdout emptiness and the `fatal:` line are).

use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-daemon-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Fixture { root }
    }

    /// Returns `(exit code, stderr)`; stdout is asserted empty, which holds for
    /// every `daemon` diagnostic — all of them go to stderr.
    fn run(&self, args: &[&str]) -> (i32, String) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            out.stdout.is_empty(),
            "`git {args:?}` wrote to stdout: {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
        (
            out.status.code().expect("daemon exited rather than being signalled"),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

/// `--port=` is validated with `strtoul(v, &end, 0)` and only has to be fully
/// consumed — the range check is left to the resolver. `99999` therefore parses,
/// reaches `getaddrinfo`, fails there, and produces an empty socket list, which
/// is what `serve()` turns into a `die`.
#[test]
fn a_port_outside_the_range_fails_in_resolution_not_parsing() {
    let f = Fixture::new("bigport");
    let (code, stderr) = f.run(&["daemon", "--port=99999"]);
    assert_eq!(code, 128, "stderr was: {stderr}");
    assert!(
        stderr.ends_with("fatal: unable to allocate any listen sockets on port 99999\n"),
        "unexpected refusal: {stderr}"
    );
    // The per-address complaint that precedes it: `logerror` stamps the pid, and
    // the wildcard host is a NULL pointer in the C, printed as `(null)`.
    assert!(
        stderr.contains("getaddrinfo() for (null) failed:"),
        "the resolution failure should be logged first: {stderr}"
    );
}

/// With an explicit `--listen=`, the same failure adds the per-host line
/// `socksetup` logs when one named address yields no sockets, and the host is
/// named rather than `(null)`.
#[test]
fn a_named_listen_address_is_reported_by_name() {
    let f = Fixture::new("listen");
    let (code, stderr) = f.run(&["daemon", "--listen=127.0.0.1", "--port=99999"]);
    assert_eq!(code, 128, "stderr was: {stderr}");
    assert!(
        stderr.contains("getaddrinfo() for 127.0.0.1 failed:")
            && stderr.contains(
                "unable to allocate any listen sockets for host 127.0.0.1 on port 99999"
            )
            && stderr.ends_with("fatal: unable to allocate any listen sockets on port 99999\n"),
        "unexpected refusal: {stderr}"
    );
}

/// Under `--syslog` the die routine is swapped for `daemon_die`, which logs to
/// syslog and exits **1**, and stderr has already been redirected to /dev/null —
/// so the same failure is silent with a different status. This is the one place
/// where `git daemon` does not use git's usual 128.
#[test]
fn syslog_swaps_the_die_routine_for_exit_one() {
    let f = Fixture::new("syslog");
    assert_eq!(
        f.run(&["daemon", "--syslog", "--port=99999"]),
        (1, String::new())
    );
}

/// `--log-destination=none` silences stderr the same way but does *not* swap the
/// die routine: the status stays 128.
#[test]
fn log_destination_none_silences_without_changing_the_status() {
    let f = Fixture::new("lognone");
    assert_eq!(
        f.run(&["daemon", "--log-destination=none", "--port=99999"]),
        (128, String::new())
    );
}

/// The startup checks run before any socket work, so an incompatible pair is
/// reported even though the port would also have failed. `--inetd` defaults the
/// log destination to syslog, which is what makes this refusal silent and 1
/// rather than a `fatal:` line and 128 — the two rules compose.
#[test]
fn startup_checks_precede_the_sockets() {
    let f = Fixture::new("checks");
    assert_eq!(f.run(&["daemon", "--inetd", "--port=99999"]), (1, String::new()));
    // Spelled the other way, with the destination pinned back to stderr, the
    // same check is the ordinary `die`.
    assert_eq!(
        f.run(&["daemon", "--log-destination=stderr", "--inetd", "--port=99999"]),
        (
            128,
            "fatal: --listen= and --port= are incompatible with --inetd\n".to_string()
        )
    );
}
