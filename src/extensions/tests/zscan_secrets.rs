//! `git zscan` — the fleet secret scanner, and the exit code that makes it a gate.
//!
//! `zscan` had no integration test. Its unit tests check that the patterns
//! compile and match sample credentials, which is the easy half; what was
//! untested is everything that makes it usable as the pre-push / CI gate its
//! own documentation advertises:
//!
//!  * that it **exits non-zero when it finds something** — a gate that reports
//!    hits and exits 0 blocks nothing, and no stdout assertion notices;
//!  * that it exits **zero on a clean tree**, or it blocks everything;
//!  * that it reads **tracked content across the indexed set**, not the
//!    working directory it happens to be run from;
//!  * that it skips binary files, which is what keeps a scan of a repository
//!    full of packs from drowning in false positives.
//!
//! The fixture plants one credential of each kind that the patterns name, in a
//! second repository, so a scan run from neither repo has to find it through
//! the index.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?} failed");
}

/// Run a superset verb against the fixture's own daemon state; returns
/// `(stdout+stderr, exited-zero)`.
fn zvcs(home: &Path, sock: &Path, args: &[&str]) -> (String, bool) {
    let out = Command::new(BIN)
        .args(args)
        .env("ZVCS_HOME", home)
        .env("ZVCS_SOCK", sock)
        .output()
        .unwrap();
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    (s, out.status.success())
}

#[test]
fn zscan_finds_planted_credentials_and_exits_non_zero() {
    let root = std::env::temp_dir().join(format!("zvcs-zscan-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let work = root.join("work");
    let home = root.join("home");
    let sock = root.join("sock");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(&home).unwrap();

    // A clean repository, and one carrying secrets.
    let clean = work.join("clean");
    std::fs::create_dir_all(&clean).unwrap();
    git(&clean, &["init", "-q", "-b", "main", "."]);
    std::fs::write(clean.join("src.txt"), "let x = 1; // nothing to see\n").unwrap();
    git(&clean, &["add", "-A"]);
    git(&clean, &["commit", "-qm", "clean"]);

    let leaky = work.join("leaky");
    std::fs::create_dir_all(&leaky).unwrap();
    git(&leaky, &["init", "-q", "-b", "main", "."]);
    // One line per pattern the scanner names. The AWS key is the documented
    // example value, not a real credential.
    std::fs::write(
        leaky.join("config.txt"),
        "aws = AKIAIOSFODNN7EXAMPLE\n\
         api_key = \"abcdef0123456789ABCDEF\"\n\
         token: ghp_0123456789abcdefghijklmnopqrstuvwxyz\n",
    )
    .unwrap();
    std::fs::write(leaky.join("id_rsa"), "-----BEGIN RSA PRIVATE KEY-----\nMIIEow==\n").unwrap();
    // A binary file whose bytes happen to contain a key pattern: skipped, like
    // `git grep` skips binary, so it must not appear in the output.
    let mut blob: Vec<u8> = b"AKIAIOSFODNN7EXAMPLE".to_vec();
    blob.push(0);
    blob.extend_from_slice(b"binary tail");
    std::fs::write(leaky.join("packed.bin"), &blob).unwrap();
    git(&leaky, &["add", "-A"]);
    git(&leaky, &["commit", "-qm", "leaky"]);

    // Index both repositories, so the scan runs over the fleet rather than a cwd.
    let (idx, _) = zvcs(&home, &sock, &["zreindex", "--sync", work.to_str().unwrap()]);
    assert!(idx.contains("indexed 2"), "both repos indexed:\n{idx}");

    let (out, ok) = zvcs(&home, &sock, &["zscan"]);
    assert!(!ok, "zscan found secrets and still exited zero — it gates nothing:\n{out}");

    // Every planted credential, named by its pattern.
    for (pattern, needle) in [
        ("aws-access-key", "AKIAIOSFODNN7EXAMPLE"),
        ("private-key", "BEGIN RSA PRIVATE KEY"),
        ("github-token", "ghp_0123456789"),
        ("generic-secret", "api_key"),
    ] {
        assert!(out.contains(pattern), "{pattern} not reported:\n{out}");
        assert!(out.contains(needle), "the {pattern} line is missing its content:\n{out}");
    }

    // The binary file is skipped even though its bytes match.
    assert!(!out.contains("packed.bin"), "a binary file was scanned:\n{out}");
    // The clean repository contributes nothing.
    assert!(!out.contains("src.txt"), "the clean repo produced a hit:\n{out}");
    // And the summary counts what it printed.
    assert!(out.contains("potential secret(s) across 2 repos"), "no summary line:\n{out}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn zscan_exits_zero_when_the_tree_is_clean() {
    // The other half of the gate. A scanner that always exits non-zero is as
    // useless as one that never does, and only this direction proves the exit
    // code tracks the finding rather than the run.
    let root = std::env::temp_dir().join(format!("zvcs-zscan-clean-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let work = root.join("work");
    let home = root.join("home");
    let sock = root.join("sock");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(&home).unwrap();

    let repo = work.join("clean");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main", "."]);
    // Deliberately close to the patterns without matching: a short key-shaped
    // value, and an AKIA prefix that is too short to be a key.
    std::fs::write(repo.join("notes.txt"), "api_key = \"short\"\nAKIA123\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "clean"]);

    let (idx, _) = zvcs(&home, &sock, &["zreindex", "--sync", work.to_str().unwrap()]);
    assert!(idx.contains("indexed 1"), "{idx}");

    let (out, ok) = zvcs(&home, &sock, &["zscan"]);
    assert!(ok, "a clean tree failed the scan:\n{out}");
    assert!(out.contains("0 potential secret(s)"), "expected a zero count:\n{out}");

    let _ = std::fs::remove_dir_all(&root);
}
