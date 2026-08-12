//! `git version --build-options` — `get_version_info(buf, 1)`.
//!
//! The build report is the one command whose output is a set of claims about
//! the binary printing it, which makes it the easiest place in the port to lie
//! and the hardest place to notice a lie. Stock's values are known
//! (`libcurl: 8.7.1`, `zlib: 1.2.12`, `SHA-1: SHA1_DC`, `gettext: enabled`,
//! `SHA-256: SHA256_BLK`) and pasting them in would turn a visibly-failing
//! parity case into a silently-passing fabrication.
//!
//! So the assertions here are deliberately one-sided. They do not pin the exact
//! text of the honest lines — those may legitimately change when the build does.
//! They pin the two things that must never change:
//!
//!   * every line that *is* printed is derived from this build (the two `sizeof`
//!     lines are recomputed here from the same target types), and
//!   * no line names a C component this build does not link. If `libcurl:` ever
//!     reappears, either the binary started linking curl — in which case this
//!     test should be updated along with the code — or someone copied stock's
//!     report, which is the failure this file exists to catch.
//!
//! `git diagnose` and `git bugreport` embed the same block through the same
//! function, as `cmd_diagnose()` does in git, so the report is asserted to be
//! identical in `git bugreport`'s output too.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Components git names only when it links them. None is present in this build:
/// the transport is reqwest + rustls, there are no message catalogs, deflate is
/// in-tree and inflate is `zlib-rs`, and `sha2` is not in the dependency graph.
const NOT_LINKED: [&str; 6] = ["gettext:", "libcurl:", "OpenSSL:", "zlib:", "zlib-ng:", "SHA-256:"];

fn fixture(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-buildopts-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("ZVCS_HOME", dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("TERM", "dumb")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

fn stdout(out: &Output) -> String {
    assert!(out.status.success(), "exit {:?}", out.status.code());
    String::from_utf8(out.stdout.clone()).unwrap()
}

fn field<'a>(report: &'a str, name: &str) -> Option<&'a str> {
    report
        .lines()
        .find_map(|l| l.strip_prefix(name)?.strip_prefix(' '))
}

/// "The format of this string should be kept stable for compatibility with
/// external projects that rely on the output of `git version`" — so the build
/// report opens with exactly what plain `git version` prints, and
/// `--no-build-options` leaves nothing but that line.
#[test]
fn the_report_opens_with_the_plain_version_line() {
    let dir = fixture("shape");
    let plain = stdout(&run(&dir, &["version"]));
    let report = stdout(&run(&dir, &["version", "--build-options"]));

    assert!(report.starts_with(&plain), "report must begin with {plain:?}");
    assert!(report.len() > plain.len(), "--build-options added nothing");
    assert_eq!(stdout(&run(&dir, &["version", "--no-build-options"])), plain);
}

/// The two width lines are facts about the target this was compiled for, and
/// are recomputed here from the same types the port reads (`c_long`, `usize`).
/// A hardcoded `8` would pass on the machines this is usually built on and lie
/// on any other.
#[test]
fn the_sizes_are_this_targets_sizes() {
    let dir = fixture("sizes");
    let report = stdout(&run(&dir, &["version", "--build-options"]));

    assert_eq!(
        field(&report, "sizeof-long:"),
        Some(std::mem::size_of::<std::os::raw::c_long>().to_string().as_str())
    );
    assert_eq!(
        field(&report, "sizeof-size_t:"),
        Some(std::mem::size_of::<usize>().to_string().as_str())
    );
}

/// No line may name a C component this build does not link. This is the
/// anti-fabrication assertion: stock prints all six of these, so their absence
/// is the difference between a report about *this* binary and a copy of stock's.
#[test]
fn no_line_claims_a_component_this_build_does_not_link() {
    let dir = fixture("honest");
    let report = stdout(&run(&dir, &["version", "--build-options"]));

    for claim in NOT_LINKED {
        assert!(
            !report.lines().any(|l| l.starts_with(claim)),
            "report claims {claim} which this build does not link:\n{report}"
        );
    }
    // The lines that *are* printed say what is true here rather than what stock
    // says: this binary is Rust, and its SHA-1 is a crate, not a C backend.
    assert_eq!(field(&report, "rust:"), Some("enabled"));
    assert_eq!(field(&report, "SHA-1:"), Some("sha1-checked"));
}

/// `porcelain::init` rejects `--ref-format=reftable` and
/// `--object-format=sha256`, so these two lines are not merely the defaults —
/// they are the only formats this build has a backend for, and the report must
/// agree with the command that enforces it.
#[test]
fn the_declared_defaults_are_the_formats_this_build_supports() {
    let dir = fixture("formats");
    let report = stdout(&run(&dir, &["version", "--build-options"]));

    assert_eq!(field(&report, "default-ref-format:"), Some("files"));
    assert_eq!(field(&report, "default-hash:"), Some("sha1"));

    for rejected in [["init", "--ref-format=reftable"], ["init", "--object-format=sha256"]] {
        let out = run(&dir, &rejected);
        assert!(
            !out.status.success(),
            "{rejected:?} succeeded, so the report's default is not the only supported format"
        );
    }
}

/// `cmd_diagnose()` renders its version block with `get_version_info(&buf, 1)`,
/// the same call `git version --build-options` makes. `git bugreport` prints
/// that block under its `git version:` header, so the report must appear there
/// verbatim — one implementation, three commands.
#[test]
fn bugreport_embeds_the_same_report() {
    let dir = fixture("bugreport");
    let report = stdout(&run(&dir, &["version", "--build-options"]));

    let out = run(&dir, &["bugreport", "--no-suffix"]);
    assert!(out.status.success(), "bugreport failed: {out:?}");
    let text = std::fs::read_to_string(dir.join("git-bugreport.txt")).unwrap();

    assert!(
        text.contains(&format!("git version:\n{report}")),
        "bugreport's version block is not the build report:\n{text}"
    );
}
