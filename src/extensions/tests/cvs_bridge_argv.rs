//! The argument surfaces of the two CVS bridges, which are the parts of them
//! that run before any `cvs` binary or CVS sandbox is involved.
//!
//! Both are Perl scripts in stock git, and both take their argument handling
//! from a Perl module rather than from git's own `parse_options` — so their
//! diagnostics follow those modules' rules, not git's, and the differences are
//! exactly what a from-scratch reimplementation gets wrong:
//!
//! * `git-cvsexportcommit` uses `Getopt::Std::getopts('uhPpvcfkam:d:w:W')`. An
//!   unknown character is a **warning**, not a death: parsing continues through
//!   the rest of the cluster, so `--no-such-flag` produces one line per
//!   character outside the set *and still acts on the `h` inside it*. Option
//!   scanning also stops dead at the first argument that is not `-<something>`.
//! * `git-cvsserver` uses `Getopt::Long`, whose defaults are case-insensitive
//!   matching with unique-prefix abbreviation, and which collects every
//!   complaint before dying once with the usage block. Its `-h` is not help at
//!   all: `@opts` declares `'h|H'` (setting `$state->{h}`) while the guard reads
//!   `$state->{help}`, so `-h` falls through to the request loop, reads a closed
//!   stdin, and exits **0** having printed nothing.
//!
//! Every expectation is what stock git 2.55.0 printed for the same argv, taken
//! from running the stock binary, and is hardcoded so the suite needs no `git`
//! — and no CVS — on the machine running it. Nothing here spawns `cvs`, opens a
//! socket or touches a sandbox: stdin is closed for every case, which is what
//! keeps the request loop's end-of-input path the one that runs.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// `sub usage` in `git-cvsexportcommit`, verbatim.
const EXPORT_USAGE: &str = "usage: GIT_DIR=/path/to/.git git cvsexportcommit [-h] [-p] [-v] [-c] [-f] [-u] [-k] [-w cvsworkdir] [-m msgprefix] [ parent ] commit\n";

/// `$usage` in `git-cvsserver`, verbatim.
const SERVER_USAGE: &str = concat!(
    "usage: git cvsserver [options] [pserver|server] [<directory> ...]\n",
    "    --base-path <path>  : Prepend to requested CVSROOT\n",
    "                          Can be read from GIT_CVSSERVER_BASE_PATH\n",
    "    --strict-paths      : Don't allow recursing into subdirectories\n",
    "    --export-all        : Don't check for gitcvs.enabled in config\n",
    "    --version, -V       : Print version information and exit\n",
    "    -h, -H              : Print usage information and exit\n",
    "\n",
    "<directory> ... is a list of allowed directories. If no directories\n",
    "are given, all are allowed. This is an additional restriction, gitcvs\n",
    "access still needs to be enabled by the gitcvs.enabled config option.\n",
    "Alternately, one directory may be specified in GIT_CVSSERVER_ROOT.\n",
);

struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-cvsargv-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    /// Run with stdin closed. Returns `(exit code, stderr)`; stdout is asserted
    /// empty, which holds for every case in this file.
    fn run(&self, args: &[&str]) -> (i32, String) {
        self.run_with_stdin(args, None)
    }

    fn run_with_stdin(&self, args: &[&str], input: Option<&str>) -> (i32, String) {
        let mut child = self
            .cmd(args)
            .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(input) = input {
            child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        }
        let out = child.wait_with_output().unwrap();
        assert!(
            out.stdout.is_empty(),
            "`git {args:?}` wrote to stdout: {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
        (
            out.status.code().expect("the command exited rather than being signalled"),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

// ---------------------------------------------------------------------------
// cvsexportcommit — Getopt::Std
// ---------------------------------------------------------------------------

/// `-h` is in the option set, so it sets `$opt_h` and `usage()` runs: the usage
/// line on **stderr**, exit 1.
#[test]
fn cvsexportcommit_dash_h_prints_usage_on_stderr() {
    let f = Fixture::new("exp-h");
    assert_eq!(f.run(&["cvsexportcommit", "-h"]), (1, EXPORT_USAGE.to_string()));
}

/// A long option is read as a cluster of single characters. Each character
/// outside the set warns and parsing goes on — and because `--no-such-flag`
/// contains `u`, `c`, `h`, `f` and `a`, all of which *are* in the set, the `h`
/// still triggers the usage exit.
#[test]
fn cvsexportcommit_long_option_is_a_cluster_of_warnings() {
    let f = Fixture::new("exp-long");
    let expected = format!(
        "Unknown option: -\nUnknown option: n\nUnknown option: o\nUnknown option: -\n\
         Unknown option: s\nUnknown option: -\nUnknown option: l\nUnknown option: g\n{EXPORT_USAGE}"
    );
    assert_eq!(f.run(&["cvsexportcommit", "--no-such-flag"]), (1, expected));
}

/// The same rule in the small: one warning for `x`, and the `h` after it is
/// still honoured.
#[test]
fn cvsexportcommit_unknown_character_does_not_stop_the_cluster() {
    let f = Fixture::new("exp-xh");
    assert_eq!(
        f.run(&["cvsexportcommit", "-xh"]),
        (1, format!("Unknown option: x\n{EXPORT_USAGE}"))
    );
}

/// A value option with nothing left to take is *not* a diagnostic in
/// `Getopt::Std` — it only bumps an error count the script never reads. The run
/// therefore continues and dies on the missing commit instead, with perl's
/// `die` status.
#[test]
fn cvsexportcommit_missing_option_value_is_silent() {
    let f = Fixture::new("exp-m");
    assert_eq!(
        f.run(&["cvsexportcommit", "-m"]),
        (255, "Need at least one commit identifier!\n".to_string())
    );
}

/// No arguments at all: `die "Need at least one commit identifier!"`, exit 255.
#[test]
fn cvsexportcommit_without_a_commit_dies() {
    let f = Fixture::new("exp-none");
    assert_eq!(
        f.run(&["cvsexportcommit"]),
        (255, "Need at least one commit identifier!\n".to_string())
    );
}

/// `getopts`' loop guard fails at the first argument that is not `-<something>`,
/// so a `-h` *after* an operand is an operand too and prints no usage. What the
/// command does next needs a CVS checkout, so it refuses — but it must not
/// refuse by printing help.
#[test]
fn cvsexportcommit_stops_scanning_at_the_first_operand() {
    let f = Fixture::new("exp-operand");
    let (code, stderr) = f.run(&["cvsexportcommit", "somecommit", "-h"]);
    assert_ne!(code, 0, "exporting to a CVS checkout cannot succeed here");
    assert!(
        !stderr.contains("usage: GIT_DIR="),
        "`-h` after an operand is an operand, not help: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// cvsserver — Getopt::Long, then the request loop
// ---------------------------------------------------------------------------

/// The whole point of the `$state->{h}` / `$state->{help}` mismatch: `-h` prints
/// nothing, falls into the request loop, and ends on a closed stdin with 0.
#[test]
fn cvsserver_dash_h_is_not_help_and_exits_zero() {
    let f = Fixture::new("srv-h");
    assert_eq!(f.run(&["cvsserver", "-h"]), (0, String::new()));
}

/// A client that connects and sends nothing is not an error: the loop body never
/// runs and the script exits 0 in silence.
#[test]
fn cvsserver_with_no_request_exits_zero() {
    let f = Fixture::new("srv-eof");
    assert_eq!(f.run(&["cvsserver"]), (0, String::new()));
    // `server` names the transport and is dropped from the operand list.
    assert_eq!(f.run(&["cvsserver", "server"]), (0, String::new()));
}

/// `Getopt::Long` warns per bad option and then dies with the usage block, all
/// on stderr, with perl's `die` status.
#[test]
fn cvsserver_unknown_option_warns_then_dies_with_usage() {
    let f = Fixture::new("srv-bad");
    assert_eq!(
        f.run(&["cvsserver", "--no-such-flag"]),
        (255, format!("Unknown option: no-such-flag\n{SERVER_USAGE}"))
    );
}

/// The one operand check that runs before the loop. Its message ends in a
/// newline, so perl appends no ` at <script> line <n>.` suffix.
#[test]
fn cvsserver_export_all_needs_a_directory_list() {
    let f = Fixture::new("srv-exportall");
    assert_eq!(
        f.run(&["cvsserver", "--export-all"]),
        (
            255,
            "--export-all can only be used together with an explicit '<directory>...' list\n"
                .to_string()
        )
    );
    // With a directory it gets past the check and into the loop, which ends on
    // the closed stdin.
    assert_eq!(f.run(&["cvsserver", "--export-all", "."]), (0, String::new()));
}

/// A request line that is not in the `$methods` table is `die("Unknown command
/// $_")` — the request loop's other exit, and the only one that reports a
/// client error.
#[test]
fn cvsserver_unknown_request_dies() {
    let f = Fixture::new("srv-unknown");
    let (code, stderr) = f.run_with_stdin(&["cvsserver"], Some("bogus-request\n"));
    assert_eq!(code, 255);
    assert!(
        stderr.starts_with("Unknown command bogus-request"),
        "unexpected refusal: {stderr}"
    );
}

/// A request that *is* in the table needs the revision database that is not
/// ported. It must say so rather than answer — a wrong answer here corrupts the
/// client's sandbox, which cannot be undone by a later fix.
#[test]
fn cvsserver_known_request_reports_the_gap_instead_of_answering() {
    let f = Fixture::new("srv-known");
    let (code, stderr) = f.run_with_stdin(&["cvsserver"], Some("Root /somewhere\n"));
    assert_ne!(code, 0, "an unimplemented request must not report success");
    assert!(stderr.contains("not ported"), "unexpected refusal: {stderr}");
}

/// `pserver` starts with an authentication header, and anything else — including
/// a client that sends nothing, where perl's undef interpolates as the empty
/// string — is the same refusal.
#[test]
fn cvsserver_pserver_requires_the_auth_header() {
    let f = Fixture::new("srv-pserver");
    assert_eq!(
        f.run(&["cvsserver", "pserver"]),
        (
            255,
            "E Do not understand  - expecting BEGIN AUTH REQUEST\n".to_string()
        )
    );
}
