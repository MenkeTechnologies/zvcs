//! Byte-for-byte parity tests for `git for-each-ref` version sorting
//! (`--sort=version:<key>` / `v:<key>`) and `--stdin`, checked against the
//! system git so the ported `versioncmp` and stdin-pattern reader can only pass
//! when they agree with real git.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap()
            .success(),
        "git {args:?} failed"
    );
}

/// Run `program` in `dir`, optionally feeding `stdin`, returning captured stdout.
fn run(program: &str, dir: &Path, args: &[&str], stdin: Option<&str>) -> String {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if stdin.is_some() {
        cmd.stdin(Stdio::piped());
    }
    let mut child = cmd.spawn().unwrap();
    if let Some(s) = stdin {
        child.stdin.take().unwrap().write_all(s.as_bytes()).unwrap();
    }
    let out = child.wait_with_output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A repo with a spread of version-shaped tags that exercise numeric width
/// (`v1.9` vs `v1.10`), patch components, and prerelease suffixes.
fn repo(name: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    git(
        &root,
        &[
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=t",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "c0",
        ],
    );
    for tag in [
        "v1.0", "v1.0.1", "v1.9", "v1.10", "v1.0-rc1", "v1.0-rc2", "v2.0", "v10.0",
    ] {
        git(&root, &["tag", tag]);
    }
    root
}

#[test]
fn version_sort_matches_git() {
    let root = repo("zvcs-fer-vsort");
    let args = [
        "for-each-ref",
        "--sort=version:refname",
        "--format=%(refname:short)",
        "refs/tags/",
    ];
    let want = run("git", &root, &args, None);
    let got = run(BIN, &root, &args, None);
    assert_eq!(got, want, "version:refname sort mismatch");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn version_sort_short_prefix_and_descending_match_git() {
    let root = repo("zvcs-fer-vsort2");
    // `v:` is the short spelling; a leading `-` reverses.
    let args = [
        "for-each-ref",
        "--sort=-v:refname",
        "--format=%(refname:short)",
        "refs/tags/",
    ];
    let want = run("git", &root, &args, None);
    let got = run(BIN, &root, &args, None);
    assert_eq!(got, want, "-v:refname sort mismatch");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn stdin_patterns_match_git() {
    let root = repo("zvcs-fer-stdin");
    let args = ["for-each-ref", "--stdin", "--format=%(refname)"];
    // Two explicit patterns plus a trailing CRLF line git's strbuf_getline trims.
    let input = "refs/tags/v1.0\nrefs/tags/v2.0\r\n";
    let want = run("git", &root, &args, Some(input));
    let got = run(BIN, &root, &args, Some(input));
    assert_eq!(got, want, "--stdin pattern reading mismatch");
    assert!(
        got.contains("refs/tags/v1.0") && got.contains("refs/tags/v2.0"),
        "expected both stdin patterns to match, got:\n{got}"
    );
    let _ = std::fs::remove_dir_all(&root);
}
