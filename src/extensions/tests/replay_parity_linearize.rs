//! `git replay --linearize`, new in git 2.56.
//!
//! replay.c:455-485 (2.56.0): a merge commit in the range is dropped instead of
//! refused, every non-merge commit is picked onto the commit replayed last, and
//! a ref that pointed at a dropped merge moves to that last commit.
//! builtin/replay.c:137-138 and replay.c:414-418 add the two refusals. Every
//! object id and message below was measured against stock git 2.56.0 on this
//! exact fixture.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@x")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .unwrap()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = run(dir, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn commit(repo: &Path, name: &str) {
    std::fs::write(repo.join(name), format!("{name}\n")).unwrap();
    git(repo, &["add", name]);
    git(repo, &["commit", "-q", "-m", name]);
}

/// `main` is a-e. `feat` is a-b, merges `side` (a-c) as `M`, then adds d.
/// `mbr` and the tag `mtag` sit on `M`.
fn fixture(tag: &str) -> PathBuf {
    let repo = std::env::temp_dir().join(format!("zvcs-replay-linearize-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&repo);
    std::fs::create_dir_all(&repo).unwrap();
    let repo = repo.canonicalize().unwrap();
    git(&repo, &["init", "-q", "-b", "main", "."]);
    commit(&repo, "a");
    git(&repo, &["branch", "feat"]);
    git(&repo, &["branch", "side"]);
    commit(&repo, "e");
    git(&repo, &["checkout", "-q", "feat"]);
    commit(&repo, "b");
    git(&repo, &["checkout", "-q", "side"]);
    commit(&repo, "c");
    git(&repo, &["checkout", "-q", "feat"]);
    git(&repo, &["merge", "-q", "--no-ff", "-m", "M", "side"]);
    git(&repo, &["tag", "mtag"]);
    git(&repo, &["branch", "mbr"]);
    commit(&repo, "d");
    git(&repo, &["checkout", "-q", "main"]);
    repo
}

const FEAT_OLD: &str = "8c19f454cf36f0329411d23f24ce18b5960a8f27";
const MERGE: &str = "ac0b5810a4f76e375a57538324d31d4f37c53244";
const MAIN: &str = "a07a250b1df36124279dd374aa8df9f2f90a9eca";
/// b, c, d stacked on `main` with the merge gone.
const FEAT_NEW: &str = "2d87e0d7ce69ad1dc94b036142e6f95e37f6605e";
/// The linear `c` — where a ref on the dropped merge lands.
const C_NEW: &str = "aa6d1373c70245e2f56ae9e6130e865b915b3254";

#[test]
fn linearize_flattens_the_merge_out_of_the_branch() {
    let repo = fixture("flat");
    assert_eq!(git(&repo, &["rev-parse", "feat", "mbr", "main"]), format!("{FEAT_OLD}\n{MERGE}\n{MAIN}\n"));

    let out = git(&repo, &["replay", "--linearize", "--ref-action=print", "--onto", "main", "main..feat"]);
    assert_eq!(out, format!("update refs/heads/feat {FEAT_NEW} {FEAT_OLD}\n"));

    // The default ref action writes the same update, and the result is linear.
    git(&repo, &["replay", "--linearize", "--onto", "main", "main..feat"]);
    assert_eq!(git(&repo, &["rev-parse", "feat"]), format!("{FEAT_NEW}\n"));
    assert_eq!(git(&repo, &["log", "--format=%s %p", "feat"]), "d aa6d137\nc 6379e48\nb a07a250\ne 05b3b0a\na \n");
}

#[test]
fn a_ref_on_the_dropped_merge_moves_to_the_last_replayed_commit() {
    let repo = fixture("mref");
    let out = git(&repo, &["replay", "--linearize", "--ref-action=print", "--onto", "main", "main..mbr"]);
    assert_eq!(out, format!("update refs/heads/mbr {C_NEW} {MERGE}\n"));

    // A detached HEAD on the merge is decorated the same way.
    git(&repo, &["checkout", "-q", "--detach", "mtag"]);
    let out = git(&repo, &["replay", "--linearize", "--ref-action=print", "--onto", "main", "main..HEAD"]);
    assert_eq!(out, format!("update HEAD {C_NEW} {MERGE}\n"));
}

#[test]
fn without_linearize_the_merge_is_still_refused() {
    let repo = fixture("refuse");
    for args in [
        &["replay", "--ref-action=print", "--onto", "main", "main..feat"][..],
        &["replay", "--linearize", "--no-linearize", "--ref-action=print", "--onto", "main", "main..feat"][..],
    ] {
        let out = run(&repo, args);
        assert_eq!(out.status.code(), Some(128), "{args:?}");
        assert_eq!(String::from_utf8_lossy(&out.stderr), "fatal: replaying merge commits is not supported yet!\n");
    }
}

#[test]
fn linearize_refusals() {
    let repo = fixture("refusals");
    let cases: [(&[&str], i32, &str); 4] = [
        (
            &["replay", "--linearize", "--ref-action=print", "--onto", "main", "main..feat", "main..side"],
            128,
            "error: '--linearize' cannot be used with multiple branches\n",
        ),
        (
            &["replay", "--contained", "--linearize", "--onto", "main", "main..feat"],
            128,
            "fatal: options '--linearize' and '--contained' cannot be used together\n",
        ),
        (
            &["replay", "--linearize=1", "--onto", "main", "main..feat"],
            129,
            "error: option `linearize' takes no value\n",
        ),
        (
            &["replay", "--no-linearize=3", "--onto", "main", "main..feat"],
            129,
            "error: option `no-linearize' takes no value\n",
        ),
    ];
    for (args, code, stderr) in cases {
        let out = run(&repo, args);
        assert_eq!(out.status.code(), Some(code), "{args:?}");
        assert_eq!(String::from_utf8_lossy(&out.stderr), stderr, "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
    }
    // A single branch with `--ref` is allowed and replays linearly.
    let out = git(&repo, &["replay", "--linearize", "--ref-action=print", "--ref", "refs/heads/x", "--onto", "main", "main..feat"]);
    assert_eq!(out, format!("update refs/heads/x {FEAT_NEW} 0000000000000000000000000000000000000000\n"));
}

#[test]
fn usage_lists_linearize() {
    let repo = fixture("usage");
    let out = run(&repo, &["replay", "-h"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("       [--ref=<ref>] [--ref-action=<mode>] [--linearize] <revision-range>\n"), "{stdout}");
    assert!(stdout.contains("    --[no-]linearize      drop merge commits, replaying only non-merge commits\n"), "{stdout}");
}
