//! `git apply` honors `apply.whitespace` as the default `--whitespace` action,
//! with the command line overriding it. These tests pin zvcs to stock git
//! byte-for-byte (stdout + stderr + exit code + resulting worktree) across a
//! config default, a CLI override of that default, and an invalid config value.
//!
//! Only the values whose behavior `apply.rs` implements are asserted to match
//! git's applied bytes: `warn`/`nowarn` are a no-op (this port emits no
//! whitespace warnings), and an invalid value is git's exact fatal at
//! config-read time. The byte-altering actions (`fix`/`strip`/`error`) are not
//! implemented, so they are only checked to override cleanly when the CLI
//! supplies a `nowarn`, never asserted to reproduce git's fixing.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

/// The base content the patch is built against, restored before each apply run
/// so zvcs and git start from an identical worktree.
const BASE: &str = "line1\nline2\n";

/// A unified diff that appends a line carrying trailing whitespace — the input
/// that makes git's whitespace machinery observable. The added line ends in
/// three spaces before its newline.
const WS_PATCH: &str = concat!(
    "--- a/f.txt\n",
    "+++ b/f.txt\n",
    "@@ -1,2 +1,3 @@\n",
    " line1\n",
    " line2\n",
    "+line3   \n",
);

/// A unified diff with no whitespace error, to check the config-read path does
/// not disturb an ordinary apply.
const CLEAN_PATCH: &str = "\
--- a/f.txt
+++ b/f.txt
@@ -1,2 +1,3 @@
 line1
 line2
+line3
";

fn fixture(tag: &str, patch: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-applycfg-{tag}-{}", std::process::id()));
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
    std::fs::write(repo.join("f.txt"), BASE).unwrap();
    git(&repo, &["add", "f.txt"]);
    git(&repo, &["commit", "-q", "-m", "c0"]);
    std::fs::write(repo.join("p.diff"), patch).unwrap();
    (repo, home)
}

/// Restore the pristine worktree, run `<bin> apply [extra] p.diff` under a
/// byte-identical isolated environment, and return the process output plus the
/// resulting `f.txt` bytes so content, streams and exit code are all comparable.
fn run_apply(bin: &str, repo: &Path, home: &Path, extra: &[&str]) -> (Output, Vec<u8>) {
    std::fs::write(repo.join("f.txt"), BASE).unwrap();
    let mut args = vec!["apply"];
    args.extend_from_slice(extra);
    args.push("p.diff");
    let out = Command::new(bin)
        .args(&args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("ZVCS_HOME", home)
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .unwrap();
    let file = std::fs::read(repo.join("f.txt")).unwrap();
    (out, file)
}

/// Assert zvcs and stock git agree on stdout, stderr, exit code and the applied
/// file bytes for the same config + command line.
fn assert_match(repo: &Path, home: &Path, extra: &[&str], what: &str) {
    let (z, zf) = run_apply(BIN, repo, home, extra);
    let (g, gf) = run_apply("git", repo, home, extra);
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
    assert_eq!(zf, gf, "{what}: resulting f.txt bytes");
}

#[test]
fn apply_whitespace_config_nowarn_matches_git() {
    let (repo, home) = fixture("nowarn", WS_PATCH);

    // apply.whitespace=nowarn: neither git nor zvcs warns; the trailing-ws line
    // applies verbatim. Exercises the config being read as the default action.
    git(&repo, &["config", "apply.whitespace", "nowarn"]);
    assert_match(&repo, &home, &[], "config=nowarn");
    let after = std::fs::read(repo.join("f.txt")).unwrap();
    assert_eq!(after, b"line1\nline2\nline3   \n", "trailing ws preserved");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn apply_whitespace_cli_overrides_config() {
    let (repo, home) = fixture("override", WS_PATCH);

    // apply.whitespace=error would reject the patch (git) or defer as
    // unimplemented (zvcs); `--whitespace=nowarn` on the command line overrides
    // it, so both apply cleanly with the trailing whitespace intact.
    git(&repo, &["config", "apply.whitespace", "error"]);
    assert_match(&repo, &home, &["--whitespace=nowarn"], "config=error + CLI nowarn");
    let (z, _) = run_apply(BIN, &repo, &home, &["--whitespace=nowarn"]);
    assert_eq!(z.status.code(), Some(0), "override applies cleanly");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn apply_whitespace_invalid_config_is_fatal() {
    let (repo, home) = fixture("invalid", WS_PATCH);

    // An unrecognized apply.whitespace value is git's exact fatal (exit 128) at
    // config-read time — before the patch is opened.
    git(&repo, &["config", "apply.whitespace", "bogus"]);
    assert_match(&repo, &home, &[], "config=bogus");
    let (z, _) = run_apply(BIN, &repo, &home, &[]);
    assert_eq!(z.status.code(), Some(128), "invalid config exits 128");
    assert_eq!(
        String::from_utf8_lossy(&z.stderr),
        "error: unrecognized whitespace option 'bogus'\n",
        "git's exact invalid-value message"
    );

    // git parses config before the command line, so an invalid value is fatal
    // even when a valid `--whitespace` override is also present.
    assert_match(&repo, &home, &["--whitespace=nowarn"], "config=bogus beats CLI override");
    let (z, _) = run_apply(BIN, &repo, &home, &["--whitespace=nowarn"]);
    assert_eq!(z.status.code(), Some(128), "invalid config still fatal under CLI override");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn apply_whitespace_clean_patch_defaults_match_git() {
    let (repo, home) = fixture("clean", CLEAN_PATCH);

    // No config: the config-read path must leave an ordinary apply untouched.
    assert_match(&repo, &home, &[], "no config, clean patch");

    // apply.whitespace=warn on a patch with no whitespace error: git emits no
    // warning, so zvcs matches byte-for-byte (the default action is honoured as
    // a no-op).
    git(&repo, &["config", "apply.whitespace", "warn"]);
    assert_match(&repo, &home, &[], "config=warn, clean patch");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
