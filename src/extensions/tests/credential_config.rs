//! `git credential` reads its configuration through git's credential engine, and
//! zvcs reaches the same keys through the vendored `gix` credential cascade plus
//! a direct read of `credential.protectProtocol`. These tests pin the keys that
//! actually drive a code path in `credential.rs` to stock git byte-for-byte:
//!
//!   * `credential.useHttpPath`        — whether an http(s) path stays part of
//!                                        the credential context (and so whether
//!                                        `fill` echoes it back).
//!   * `credential.<url>.useHttpPath`  — the same, scoped to a matching url; a
//!                                        non-matching host must fall back to the
//!                                        default (path dropped).
//!   * `credential.username`           — a default username folded into the
//!                                        context when the helper supplies none.
//!   * `credential.<url>.username`     — the same, scoped to a matching url.
//!   * `credential.protectProtocol`    — on by default, so a carriage return in a
//!                                        credential value is a fatal protocol
//!                                        smuggling attempt.
//!
//! Every case is exercised with a fake inline helper (`credential.helper='!f() {
//! echo username=u; echo password=p; }; f'`) so nothing touches the network or an
//! OS keychain, and the interactive prompt path is never reached.
//!
//! Deliberately not covered, because they have no representation in
//! `credential.rs`: `credential.interactive` and `credential.sanitizePrompt` both
//! govern the interactive terminal prompt, which is entirely inside gix's prompt
//! layer and is never entered once a helper returns a credential — there is no
//! behavior in the port for those keys to steer.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A fake credential helper that returns a fixed username and password. The `!`
/// prefix makes git (and the gix cascade) run it through the shell, so no real
/// helper binary, network, or keychain is involved.
const HELPER_FULL: &str = "!f() { echo username=u; echo password=p; }; f";
/// A fake helper that returns only a password, leaving the username to config.
const HELPER_PW_ONLY: &str = "!f() { echo password=p; }; f";

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args(args)
            .current_dir(dir)
            // Pin every write to the temp repo. `GIT_CEILING_DIRECTORIES` stops
            // git's upward `.git` search at `dir`, so if the temp repo's `.git`
            // were ever missing (a failed `init`, a racing cleanup) `git config`
            // fails with "not a git repository" instead of walking up and writing
            // into the real zvcs repo's config — which once leaked a fake
            // credential helper and broke auth. Global/system config are neutered
            // too, so a stray write cannot escape the temp repo in any direction.
            .env("GIT_CEILING_DIRECTORIES", dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .status()
            .unwrap()
            .success(),
        "git {args:?} failed"
    );
}

/// An empty repository under an isolated `HOME`, with `user.*` set so git never
/// complains and nothing bleeds in from the developer's real config.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-credcfg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "alice@example.com"]);
    git(&repo, &["config", "user.name", "Alice"]);
    (repo, home)
}

/// Run `<bin> credential fill` with `request` on stdin under a byte-identical
/// isolated environment.
fn fill(bin: &str, repo: &Path, home: &Path, request: &str) -> Output {
    let mut child = Command::new(bin)
        .args(["credential", "fill"])
        .current_dir(repo)
        .env("HOME", home)
        // Never let discovery walk out of the temp repo into the real one.
        .env("GIT_CEILING_DIRECTORIES", repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("ZVCS_HOME", home)
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(request.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

/// Assert zvcs and stock git agree on exit code, stdout, and stderr for the same
/// `credential fill` request under the config already written to `repo`.
fn assert_match(repo: &Path, home: &Path, request: &str, what: &str) {
    let z = fill(BIN, repo, home, request);
    let g = fill("git", repo, home, request);
    assert_eq!(z.status.code(), g.status.code(), "{what}: exit code");
    assert_eq!(
        String::from_utf8_lossy(&z.stdout),
        String::from_utf8_lossy(&g.stdout),
        "{what}: stdout"
    );
    assert_eq!(
        String::from_utf8_lossy(&z.stderr),
        String::from_utf8_lossy(&g.stderr),
        "{what}: stderr"
    );
}

/// With no `credential.useHttpPath`, an http path is not part of the credential,
/// so `fill` never echoes it — the config default is "off".
#[test]
fn use_http_path_default_drops_http_path() {
    let (repo, home) = fixture("uhp-default");
    git(&repo, &["config", "credential.helper", HELPER_FULL]);
    assert_match(
        &repo,
        &home,
        "protocol=https\nhost=example.com\npath=x\n\n",
        "useHttpPath unset",
    );
}

/// `credential.useHttpPath=true` keeps the path in the credential context, so
/// `fill` echoes it back between `host` and `username`.
#[test]
fn use_http_path_true_keeps_path() {
    let (repo, home) = fixture("uhp-true");
    git(&repo, &["config", "credential.helper", HELPER_FULL]);
    git(&repo, &["config", "credential.useHttpPath", "true"]);
    assert_match(
        &repo,
        &home,
        "protocol=https\nhost=example.com\npath=x\n\n",
        "useHttpPath=true",
    );
}

/// `credential.<url>.useHttpPath=true` applies only to a request whose url
/// matches the subsection; a request to a different host falls back to the
/// default and drops the path.
#[test]
fn per_url_use_http_path_matches_only_that_host() {
    let (repo, home) = fixture("uhp-perurl");
    git(&repo, &["config", "credential.helper", HELPER_FULL]);
    git(&repo, &["config", "credential.https://example.com.useHttpPath", "true"]);
    assert_match(
        &repo,
        &home,
        "protocol=https\nhost=example.com\npath=x\n\n",
        "per-url useHttpPath matching host",
    );
    assert_match(
        &repo,
        &home,
        "protocol=https\nhost=other.com\npath=x\n\n",
        "per-url useHttpPath non-matching host",
    );
}

/// `credential.username` seeds the username when the helper returns none, and it
/// is echoed back by `fill`.
#[test]
fn username_config_folds_in() {
    let (repo, home) = fixture("user-global");
    git(&repo, &["config", "credential.helper", HELPER_PW_ONLY]);
    git(&repo, &["config", "credential.username", "alice"]);
    assert_match(
        &repo,
        &home,
        "protocol=https\nhost=example.com\n\n",
        "credential.username default",
    );
}

/// `credential.<url>.username` seeds the username for a matching url only.
#[test]
fn per_url_username_folds_in() {
    let (repo, home) = fixture("user-perurl");
    git(&repo, &["config", "credential.helper", HELPER_PW_ONLY]);
    git(&repo, &["config", "credential.https://example.com.username", "bob"]);
    assert_match(
        &repo,
        &home,
        "protocol=https\nhost=example.com\n\n",
        "per-url credential.username",
    );
}

/// `credential.protectProtocol` defaults to on, so a carriage return embedded in
/// a credential value is rejected as a protocol smuggling attempt with git's
/// exact fatal message and exit code, before any helper runs.
#[test]
fn protect_protocol_default_rejects_carriage_return() {
    let (repo, home) = fixture("protect-default");
    git(&repo, &["config", "credential.helper", HELPER_FULL]);
    assert_match(
        &repo,
        &home,
        "protocol=https\nhost=exa\rmple.com\n\n",
        "protectProtocol default rejects CR",
    );
}
