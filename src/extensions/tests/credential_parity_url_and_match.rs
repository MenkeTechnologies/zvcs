//! `git credential` reads a `url=` attribute with `credential_from_url_1()`
//! (credential.c:621-693), a byte scanner rather than a URL parser, and decides
//! which `[credential "<pattern>"]` subsections apply with
//! `urlmatch_config_entry()`'s two arms (urlmatch.c:700-715). Both are more
//! permissive than a URL library, and the difference is observable in what `fill`
//! prints:
//!
//! ```text
//!   * the scheme and host keep their case (`HTTPS://EXAMPLE.COM/X`), so an
//!     upper-case scheme is not recognised as http and its path survives;
//!   * every component is percent-decoded (`ex%41mple.com` → `exAmple.com`);
//!   * `file:///tmp/x` sets an *empty* host rather than none;
//!   * a `url=` line clears every field set before it and is itself overridden by
//!     the lines after it;
//!   * a newline anywhere in the url is refused by `check_url_component()`;
//!   * `credential.https://example.com/foo.*` applies to `/foo/bar` but not to
//!     `/foobar` (`url_match_prefix()`, urlmatch.c:570-600);
//!   * a schemeless `credential.example.com.*` applies through
//!     `match_partial_url()` (credential.c:156-170).
//! ```
//!
//! Every expectation is stock git 2.55.0's measured output, written out in full
//! rather than compared against whatever `git` a CI image ships. The helper is an
//! inline shell function, so no keychain, no network and no terminal prompt is
//! reached.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Returns a fixed credential without looking at anything, so the only thing
/// under test is the context git builds and hands it.
const HELPER: &str = "!f() { echo username=u; echo password=p; }; f";

/// An isolated `HOME` with no config of its own; `git credential` runs
/// `RUN_SETUP_GENTLY`, so no repository is needed.
fn home(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-credurl-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root.canonicalize().unwrap()
}

fn fill(home: &Path, config: &[&str], stdin: &[u8]) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.arg("-c").arg(format!("credential.helper={HELPER}"));
    for c in config {
        cmd.arg("-c").arg(c);
    }
    let mut child = cmd
        .args(["credential", "fill"])
        .current_dir(home)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CEILING_DIRECTORIES", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    child.wait_with_output().unwrap()
}

#[track_caller]
fn assert_fill(tag: &str, config: &[&str], stdin: &[u8], stdout: &str, stderr: &str, code: i32) {
    let home = home(tag);
    let out = fill(&home, config, stdin);
    assert_eq!(String::from_utf8_lossy(&out.stdout), stdout, "{tag}: stdout");
    assert_eq!(String::from_utf8_lossy(&out.stderr), stderr, "{tag}: stderr");
    assert_eq!(out.status.code(), Some(code), "{tag}: exit code");
    let _ = std::fs::remove_dir_all(&home);
}

/// The scheme and host are copied out of the url byte for byte. Because the
/// scheme is then not the literal `http`/`https` that `proto_is_http()` tests
/// for, the path is *not* dropped even without `credential.useHttpPath`.
#[test]
fn scheme_and_host_keep_their_case_and_the_path_survives() {
    assert_fill(
        "upper",
        &[],
        b"url=HTTPS://EXAMPLE.COM/X\n\n",
        "protocol=HTTPS\nhost=EXAMPLE.COM\npath=X\nusername=u\npassword=p\n",
        "",
        0,
    );
}

/// `url_decode_mem()` runs on every component, so a percent escape in the host
/// becomes the byte it names.
#[test]
fn percent_escapes_are_decoded_in_the_host() {
    assert_fill(
        "pct",
        &[],
        b"url=https://ex%41mple.com/a%2Fb\n\n",
        "protocol=https\nhost=exAmple.com\nusername=u\npassword=p\n",
        "",
        0,
    );
}

/// `if (!allow_partial_url || slash - host > 0)`: with partial urls refused the
/// host is always assigned, so a `file://` url reports an empty host rather than
/// omitting the attribute.
#[test]
fn a_file_url_reports_an_empty_host() {
    assert_fill(
        "file",
        &[],
        b"url=file:///tmp/x\n\n",
        "protocol=file\nhost=\npath=tmp/x\nusername=u\npassword=p\n",
        "",
        0,
    );
}

/// `credential_from_url()` opens with `credential_clear(c)`, so everything set
/// before the `url=` line is discarded, and the lines after it still win.
#[test]
fn a_url_line_clears_earlier_fields_and_loses_to_later_ones() {
    assert_fill(
        "clears",
        &[],
        b"username=first\npassword=second\nurl=https://example.com\n\n",
        "protocol=https\nhost=example.com\nusername=u\npassword=p\n",
        "",
        0,
    );
    assert_fill(
        "overridden",
        &[],
        b"url=https://z:y@other.org/p\nusername=uu\npassword=pp\n\n",
        "protocol=https\nhost=other.org\npath=p\nusername=uu\npassword=pp\n",
        "",
        0,
    );
}

/// The rejection rule is `!proto_end || proto_end == url` — a url with `://`
/// anywhere past offset zero is accepted however unlikely it looks, and only a
/// url without one is refused (with a `warning:` ahead of the `fatal:`).
#[test]
fn only_a_missing_scheme_is_refused() {
    assert_fill(
        "noscheme",
        &[],
        b"url=example.com\n\n",
        "",
        "warning: url has no scheme: example.com\nfatal: credential url cannot be parsed: example.com\n",
        128,
    );
}

/// `check_url_component()` (credential.c:583-595) refuses a newline in any
/// component, naming the component in the warning.
#[test]
fn a_newline_in_a_component_is_refused() {
    assert_fill(
        "newline",
        &[],
        b"url=https://example.com/a%0Ab\n\n",
        "",
        "warning: url contains a newline in its path component: https://example.com/a%0Ab\n\
         fatal: credential url cannot be parsed: https://example.com/a%0Ab\n",
        128,
    );
}

/// `credential_read()` hands each value on as a C string, so an embedded NUL
/// simply ends it — git neither sees nor diagnoses the tail.
#[test]
fn an_embedded_nul_truncates_the_value() {
    assert_fill(
        "nul",
        &[],
        b"protocol=https\nhost=example.com\nusername=a\x00b\npassword=p\n\n",
        "protocol=https\nhost=example.com\nusername=a\npassword=p\n",
        "",
        0,
    );
}

/// `url_match_prefix()`: `/foo` matches `/foo/bar` at the `/` boundary, so the
/// scoped `username` reaches the helper and comes back on the `fill` output
/// only if the helper does not override it — here the config username is what
/// the *request* carries, so its effect shows as the helper being consulted at
/// all. The discriminating pair is `/foo/bar` (matches) against `/foobar`
/// (does not).
#[test]
fn a_config_path_matches_a_subpath_but_not_a_longer_name() {
    // With `useHttpPath` on, the path stays in the credential, so the scoped
    // key's effect is visible in the printed context.
    let scoped = "credential.https://example.com/foo.useHttpPath=true";
    assert_fill(
        "subpath",
        &[scoped],
        b"url=https://example.com/foo/bar\n\n",
        "protocol=https\nhost=example.com\npath=foo/bar\nusername=u\npassword=p\n",
        "",
        0,
    );
    assert_fill(
        "foobar",
        &[scoped],
        b"url=https://example.com/foobar\n\n",
        "protocol=https\nhost=example.com\nusername=u\npassword=p\n",
        "",
        0,
    );
    assert_fill(
        "exact",
        &[scoped],
        b"url=https://example.com/foo\n\n",
        "protocol=https\nhost=example.com\npath=foo\nusername=u\npassword=p\n",
        "",
        0,
    );
}

/// A pattern `url_normalize()` rejects falls back to `match_partial_url()`,
/// which constrains only the fields the partial url set — so a bare host name
/// applies to an `https://` credential for that host, and not to another host.
#[test]
fn a_schemeless_host_pattern_matches_through_the_partial_url_fallback() {
    assert_fill(
        "partial-hit",
        &["credential.example.com.useHttpPath=true"],
        b"url=https://example.com/a/b\n\n",
        "protocol=https\nhost=example.com\npath=a/b\nusername=u\npassword=p\n",
        "",
        0,
    );
    assert_fill(
        "partial-miss",
        &["credential.other.org.useHttpPath=true"],
        b"url=https://example.com/a/b\n\n",
        "protocol=https\nhost=example.com\nusername=u\npassword=p\n",
        "",
        0,
    );
}
