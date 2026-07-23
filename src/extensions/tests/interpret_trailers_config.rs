//! `git interpret-trailers` reads its whole behavior from `trailer.*` config:
//! the dotless `trailer.separators` / `.where` / `.ifexists` / `.ifmissing`
//! defaults, the per-alias `trailer.<key-alias>.{key,where,ifexists,ifmissing}`
//! overrides, and `core.commentChar` for the block scan. Each is honored as an
//! option default that the matching CLI flag overrides.
//!
//! These are differential tests: the same message is piped through the stock
//! `git` on PATH and through the zvcs binary under test with identical repo
//! config, and stdout, stderr and exit code must agree byte-for-byte. The
//! reference is the installed git, so the assertions never hardcode expected
//! text — they demand equality with whatever git 2.55 produces.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A commit-message-shaped input with an existing trailer block, so both the
/// if-exists and if-missing paths have something to act on.
const MSG: &[u8] = b"subject line\n\nBody paragraph here.\n\nAcked-by: A <a@x.y>\nReviewed-by: B <b@x.y>\n";

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
        "setup: git {args:?} failed"
    );
}

fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-itcfg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@e.x"]);
    git(&repo, &["config", "user.name", "t"]);
    (repo, home)
}

/// Run one binary through `interpret-trailers`, feeding `MSG` on stdin.
fn run(bin: &str, repo: &Path, home: &Path, args: &[&str]) -> Output {
    let mut full = vec!["interpret-trailers"];
    full.extend_from_slice(args);
    let mut child = Command::new(bin)
        .args(&full)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(MSG).unwrap();
    child.wait_with_output().unwrap()
}

/// Assert the zvcs binary matches stock git byte-for-byte for the same repo
/// config and the same arguments.
fn assert_matches(repo: &Path, home: &Path, args: &[&str]) {
    let real = run("git", repo, home, args);
    let ours = run(BIN, repo, home, args);
    assert_eq!(
        real.stdout, ours.stdout,
        "stdout differs for args {args:?}\n real: {:?}\n ours: {:?}",
        String::from_utf8_lossy(&real.stdout),
        String::from_utf8_lossy(&ours.stdout),
    );
    assert_eq!(
        real.stderr, ours.stderr,
        "stderr differs for args {args:?}\n real: {:?}\n ours: {:?}",
        String::from_utf8_lossy(&real.stderr),
        String::from_utf8_lossy(&ours.stderr),
    );
    assert_eq!(
        real.status.code(),
        ours.status.code(),
        "exit code differs for args {args:?}"
    );
}

#[test]
fn separators_config_parses_and_overrides_default() {
    let (repo, home) = fixture("separators");
    git(&repo, &["config", "trailer.separators", ":="]);
    // The block's `key: value` lines parse under the extended separator set, and
    // a `--trailer key=value` splits on the configured `=`.
    assert_matches(&repo, &home, &["--parse"]);
    assert_matches(&repo, &home, &["--trailer", "Fixes=123"]);
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn where_config_default_and_cli_override() {
    let (repo, home) = fixture("where");
    git(&repo, &["config", "trailer.where", "after"]);
    // Config default places the new Acked-by after the existing one.
    assert_matches(&repo, &home, &["--trailer", "Acked-by: NEW"]);
    // --where end overrides the config default.
    assert_matches(&repo, &home, &["--where", "end", "--trailer", "Acked-by: NEW"]);
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn ifexists_config_default_and_cli_override() {
    let (repo, home) = fixture("ifexists");
    git(&repo, &["config", "trailer.ifexists", "replace"]);
    assert_matches(&repo, &home, &["--trailer", "Acked-by: NEW"]);
    assert_matches(
        &repo,
        &home,
        &["--if-exists", "doNothing", "--trailer", "Acked-by: NEW"],
    );
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn ifmissing_config_default_and_cli_override() {
    let (repo, home) = fixture("ifmissing");
    git(&repo, &["config", "trailer.ifmissing", "doNothing"]);
    // A token absent from the block is dropped under the config default...
    assert_matches(&repo, &home, &["--trailer", "Fixes: 99"]);
    // ...and re-added when --if-missing overrides it.
    assert_matches(
        &repo,
        &home,
        &["--if-missing", "add", "--trailer", "Fixes: 99"],
    );
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn per_alias_key_rewrites_token() {
    let (repo, home) = fixture("aliaskey");
    git(&repo, &["config", "trailer.ack.key", "Acked-by"]);
    // `ack: NEW` is rewritten to the configured `Acked-by` token.
    assert_matches(&repo, &home, &["--trailer", "ack: NEW"]);
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn per_alias_where_and_ifexists() {
    let (repo, home) = fixture("aliaswhere");
    git(&repo, &["config", "trailer.sob.key", "Signed-off-by"]);
    git(&repo, &["config", "trailer.sob.where", "after"]);
    assert_matches(&repo, &home, &["--trailer", "sob: ME"]);

    git(&repo, &["config", "trailer.rev.key", "Reviewed-by"]);
    git(&repo, &["config", "trailer.rev.ifexists", "replace"]);
    assert_matches(&repo, &home, &["--trailer", "rev: NEW"]);
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn per_alias_ifmissing() {
    let (repo, home) = fixture("aliasmissing");
    git(&repo, &["config", "trailer.fix.key", "Fixes"]);
    git(&repo, &["config", "trailer.fix.ifmissing", "doNothing"]);
    assert_matches(&repo, &home, &["--trailer", "fix: 42"]);
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn comment_char_config_drives_block_scan() {
    let (repo, home) = fixture("commentchar");
    git(&repo, &["config", "core.commentChar", ";"]);
    // Only-trailers output depends on which lines count as comments.
    assert_matches(&repo, &home, &["--parse"]);
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
